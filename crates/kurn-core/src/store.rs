//! Durable Yjs persistence (spec §6.3 "Persistence").
//! `<doc>.state` = compacted full state; `<doc>.updates` = append-only frames
//! (u32 LE length + v1 update), fsynced on every append. A torn trailing frame
//! (crash mid-write) is detected on load and truncated.
use anyhow::{Context, Result};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use yrs::updates::decoder::Decode;
use yrs::{Doc, ReadTxn, StateVector, Transact, Update};

pub struct DocStore {
    state_path: PathBuf,
    updates_path: PathBuf,
    updates: File,
    pub frames_since_compact: usize,
}

impl DocStore {
    /// Open (creating if needed) and load everything into `doc`.
    /// Returns the store and whether any persisted data existed.
    pub fn open(dir: &Path, name: &str, doc: &Doc) -> Result<(Self, bool)> {
        fs::create_dir_all(dir)?;
        let state_path = dir.join(format!("{name}.state"));
        let updates_path = dir.join(format!("{name}.updates"));
        let mut existed = false;

        if state_path.exists() {
            let bytes = fs::read(&state_path)?;
            let u = Update::decode_v1(&bytes).context("corrupt state file")?;
            doc.transact_mut().apply_update(u)?;
            existed = true;
        }
        let mut frames = 0;
        if updates_path.exists() {
            let mut buf = vec![];
            File::open(&updates_path)?.read_to_end(&mut buf)?;
            let mut off = 0usize;
            while off + 4 <= buf.len() {
                let len = u32::from_le_bytes(buf[off..off + 4].try_into().unwrap()) as usize;
                if off + 4 + len > buf.len() {
                    break; // torn frame
                }
                let u = Update::decode_v1(&buf[off + 4..off + 4 + len])
                    .context("corrupt update frame")?;
                doc.transact_mut().apply_update(u)?;
                off += 4 + len;
                frames += 1;
                existed = true;
            }
            if off != buf.len() {
                eprintln!(
                    "kurn-core: {}: dropping torn trailing frame ({} bytes)",
                    updates_path.display(),
                    buf.len() - off
                );
                OpenOptions::new()
                    .write(true)
                    .open(&updates_path)?
                    .set_len(off as u64)?;
            }
        }
        let updates = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&updates_path)?;
        Ok((
            DocStore {
                state_path,
                updates_path,
                updates,
                frames_since_compact: frames,
            },
            existed,
        ))
    }

    pub fn append(&mut self, update: &[u8]) -> Result<()> {
        let mut frame = Vec::with_capacity(update.len() + 4);
        frame.extend_from_slice(&(update.len() as u32).to_le_bytes());
        frame.extend_from_slice(update);
        self.updates.write_all(&frame)?;
        self.updates.sync_data()?;
        self.frames_since_compact += 1;
        Ok(())
    }

    /// Fold everything into the state file and truncate the update log.
    pub fn compact(&mut self, doc: &Doc) -> Result<()> {
        let bytes = doc
            .transact()
            .encode_state_as_update_v1(&StateVector::default());
        let tmp = self.state_path.with_extension("state.tmp");
        {
            let mut f = File::create(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, &self.state_path)?;
        self.updates = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.updates_path)?;
        self.updates.sync_all()?;
        self.updates = OpenOptions::new().append(true).open(&self.updates_path)?;
        self.frames_since_compact = 0;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use yrs::{GetString, Text};

    #[test]
    fn survives_torn_frame_and_compaction() {
        let dir = tempfile::tempdir().unwrap();
        let doc = Doc::new();
        let (mut st, existed) = DocStore::open(dir.path(), "t", &doc).unwrap();
        assert!(!existed);
        let txt = doc.get_or_insert_text("x");
        for w in ["a", "b", "c"] {
            let mut txn = doc.transact_mut();
            txt.push(&mut txn, w);
            let u = txn.encode_update_v1();
            drop(txn);
            st.append(&u).unwrap();
        }
        // Simulate a crash mid-append.
        OpenOptions::new()
            .append(true)
            .open(dir.path().join("t.updates"))
            .unwrap()
            .write_all(&[9, 0, 0, 0, 1])
            .unwrap();
        let d2 = Doc::new();
        let (mut st2, existed) = DocStore::open(dir.path(), "t", &d2).unwrap();
        assert!(existed);
        assert_eq!(d2.get_or_insert_text("x").get_string(&d2.transact()), "abc");
        st2.compact(&d2).unwrap();
        let d3 = Doc::new();
        DocStore::open(dir.path(), "t", &d3).unwrap();
        assert_eq!(d3.get_or_insert_text("x").get_string(&d3.transact()), "abc");
    }
}

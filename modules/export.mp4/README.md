# export.mp4 — destination presets

Packages a rendered video with ffmpeg (spec §9.4 / Phase 6).

```sh
kurn run export.mp4 --input renders/final.mp4 --preset youtube --out renders/youtube.mp4
kurn run export.mp4 --input renders/final.mp4 --preset vertical --out renders/vertical.mp4
```

| preset | size | fit | video | audio |
|---|---|---|---|---|
| `youtube` | 1920×1080 | pad | H.264, CRF 20, faststart | AAC 192k |
| `vertical` | 1080×1920 | crop | H.264, CRF 20 | AAC 160k |
| `square` | 1080×1080 | crop | H.264, CRF 20 | AAC 160k |
| `web` | 1280×720 | pad | H.264, CRF 23 | AAC 128k |

`--crf` overrides the preset's quality. Paths are relative to the project root
unless absolute. Output is returned as a file (not ingested into the library).

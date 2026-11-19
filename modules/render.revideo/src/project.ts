import { makeProject } from '@revideo/core';
import timeline from './scenes/timeline?scene';

// Injected by run.ts through Vite `define`; Revideo has no per-render fps override.
declare const __KURN_FPS__: number;
declare const __KURN_WIDTH__: number;
declare const __KURN_HEIGHT__: number;
declare const __KURN_SCALE__: number;

export default makeProject({
  scenes: [timeline],
  settings: {
    shared: { size: { x: __KURN_WIDTH__, y: __KURN_HEIGHT__ } },
    rendering: { fps: __KURN_FPS__, resolutionScale: __KURN_SCALE__ },
    preview: { fps: __KURN_FPS__ },
  },
});

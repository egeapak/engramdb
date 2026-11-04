// The graph layout, off the main thread: one message in (the graph), one
// message out (positions and edge paths), so a 1000-memory graph never
// freezes the page while it settles.

import { layout } from './layout.js';

self.onmessage = (ev) => {
  const { seq, input } = ev.data;
  try {
    self.postMessage({ seq, result: layout(input) });
  } catch (e) {
    self.postMessage({ seq, error: String((e && e.message) || e) });
  }
};

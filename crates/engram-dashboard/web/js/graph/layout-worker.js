// The graph layout, off the main thread: one message in (the graph and the
// name of the style's layout), one message out (positions and edge paths),
// so a 1000-memory graph never freezes the page while it settles.

import { runNamedLayout } from './layouts.js';

self.onmessage = (ev) => {
  const { seq, input } = ev.data;
  try {
    self.postMessage({ seq, result: runNamedLayout(input) });
  } catch (e) {
    self.postMessage({ seq, error: String((e && e.message) || e) });
  }
};

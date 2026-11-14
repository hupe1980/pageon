import { h, render } from './ui.js';
export function show() {
  render(h('h2', 'Route loaded'), h('p', 'Rendered by a lazily loaded chunk.'));
}

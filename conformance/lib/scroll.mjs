// A long page for scrolling tests: a fixed header, a button far below the fold that reports
// where it was clicked, and a scroll container.
import { html } from './pageon.mjs';

export const scrollSite = () => [
  html('https://scroll.test/', `<!doctype html><title>Scroll</title>
    <body style="margin:0">
    <div id=header style="position:fixed;top:0;left:0;right:0;height:40px;background:#ff0000"></div>
    <div style="height:3000px"></div>
    <button id=deep onclick="this.textContent = 'clicked at ' + Math.round(event.clientY) + ',' + Math.round(event.pageY)">Deep</button>
    <div id=box style="overflow:auto;width:300px;height:100px"><div style="height:500px"></div><p id=inner>inside</p></div>
    <div style="height:2000px;background:#0000ff"></div>`),
];

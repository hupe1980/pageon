// A page for screenshot tests: a red box at a known place, an image, and content
// below the fold.
import { html } from './pageon.mjs';

export const PNG_RED_2x2 = Buffer.from(
  'iVBORw0KGgoAAAANSUhEUgAAAAIAAAACCAYAAABytg0kAAAAEUlEQVR4nGP4z8DwH4QZYAwAR8oH+WdZbrcAAAAASUVORK5CYII=',
  'base64',
);

export const paintSite = () => [
  html('https://paint.test/', `<!doctype html><title>Paint</title>
    <body style="margin:0">
    <div id=red style="position:absolute;left:100px;top:50px;width:40px;height:30px;background:#ff0000"></div>
    <img id=img src="/b.png" style="position:absolute;left:300px;top:50px;width:20px;height:20px">
    <button id=buy style="position:absolute;left:10px;top:200px">Buy</button>
    <div style="position:absolute;left:0;top:1900px;width:60px;height:60px;background:#00ff00"></div>
    <div style="height:2000px"></div>`),
  { method: 'GET', url: 'https://paint.test/b.png', status: 200, headers: [['content-type', 'image/png']], body_base64: PNG_RED_2x2.toString('base64') },
];

// A board with HTML drag and drop, a pointer-driven list and a file input, for both harnesses.
import { html } from './pageon.mjs';

const board = `<title>Board</title>
  <style>section{display:block;min-height:80px;margin:8px;border:1px solid}li{display:block;height:40px}</style>
  <section id=todo aria-label=Todo><div id=c1 draggable=true>Write report</div></section>
  <section id=done aria-label=Done></section>
  <ul id=list><li id=a>Alpha</li><li id=b>Beta</li><li id=c>Gamma</li></ul>
  <input type=file id=f multiple><p id=log></p>
  <script>
    const log = t => document.getElementById('log').textContent += t + ';';
    c1.addEventListener('dragstart', e => { e.dataTransfer.setData('text/plain', 'c1'); e.dataTransfer.effectAllowed = 'move'; });
    for (const col of document.querySelectorAll('section')) {
      col.addEventListener('dragover', e => { if (col.id === 'done') e.preventDefault(); });
      col.addEventListener('drop', e => { e.preventDefault(); col.append(document.getElementById(e.dataTransfer.getData('text/plain'))); log('drop on ' + col.id); });
    }
    let dragging = null;
    list.addEventListener('pointerdown', e => { dragging = e.target; });
    document.addEventListener('pointerup', e => {
      if (!dragging) return;
      if (e.target !== dragging && e.target.parentNode === list) list.insertBefore(dragging, e.target.nextSibling);
      dragging = null;
      log('order ' + [...list.children].map(x => x.id).join(''));
    });
    f.addEventListener('change', () => log('files ' + [...f.files].map(x => x.name + ':' + x.size).join(',')));
  </script>`;

export const dragSite = () => [html('https://board.test/', board), html('https://board.test/', board)];

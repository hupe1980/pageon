// A page of web components: an open-root counter defined by script, a declarative-shadow-DOM
// card, and a closed root.
import { html } from './pageon.mjs';

export const shadowSite = () => [
  html('https://shadow.test/', `<!doctype html><title>Components</title>
    <my-counter></my-counter>
    <my-card><template shadowrootmode=open><h2><slot name=title></slot></h2><p class=body>card body</p></template>
      <span slot=title>Card title</span></my-card>
    <x-closed><template shadowrootmode=closed><button>closed</button></template></x-closed>
    <script>
      customElements.define('my-counter', class extends HTMLElement {
        constructor() {
          super();
          this.n = 0;
          this.attachShadow({ mode: 'open' }).innerHTML = '<button id=inc>Clicked 0 times</button>';
          this.shadowRoot.getElementById('inc').addEventListener('click', () => {
            this.n += 1;
            this.shadowRoot.getElementById('inc').textContent = 'Clicked ' + this.n + ' times';
          });
        }
      });
    </script>`),
];

// A sign-up form with constraint validation and a form-associated custom element, for both
// harnesses.
import { html } from './pageon.mjs';

export const formsSite = () => [
  html('https://forms.test/', `<title>Sign up</title>
    <style>.err { display: none } input:user-invalid + .err { display: block }</style>
    <form id=f action=/done>
      <label>Email <input id=email name=email type=email required></label><span class=err id=err>Email needed</span>
      <x-agree id=agree name=agree>I agree</x-agree>
      <button id=go>Sign up</button>
    </form>
    <script>
      customElements.define('x-agree', class extends HTMLElement {
        static formAssociated = true;
        constructor() {
          super();
          this.i = this.attachInternals();
          this.i.role = 'checkbox';
          this.i.ariaLabel = 'Agree';
          this.i.setValidity({valueMissing: true}, 'Please agree');
          this.addEventListener('click', () => { this.i.states.add('on'); this.i.setFormValue('yes'); this.i.setValidity({}); });
        }
      });
    </script>`),
  html('https://forms.test/done?email=a%40b.test&agree=yes', '<h1>Done</h1>'),
];

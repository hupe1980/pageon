// A page with a same-origin and a cross-origin iframe, for both harnesses.
import { html, site } from './pageon.mjs';

export const framesSite = () => site([
  html('https://shop.test/checkout', `<title>Checkout</title><h1>Checkout</h1>
    <iframe id=help name=help src=/help></iframe>
    <iframe id=pay name=pay src="https://pay.test/card" style="border: 5px solid; padding: 3px"></iframe>`),
  html('https://shop.test/help', `<p id=tip>Need help?</p><button id=ask onclick="this.textContent='asked'">Ask</button>
    <script>console.log('help frame ready')</script>`),
  html('https://pay.test/card', `<label>Card <input id=card name=card></label>
    <button id=paybtn onclick="document.querySelector('#out').textContent='paid ' + card.value">Pay</button>
    <p id=out></p>`),
]);

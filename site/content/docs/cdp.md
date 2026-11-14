+++
title = "Puppeteer & Playwright"
description = "Run existing Puppeteer and Playwright scripts on pageon over the Chrome DevTools Protocol, with the page in a sandbox and the agent view one call away."
weight = 13
[extra]
group = "Interfaces"
nav = "Puppeteer & Playwright"
+++

`pageon cdp` serves the Chrome DevTools Protocol on loopback. Existing Puppeteer and Playwright
code connects unchanged; the page still runs in pageon's sandbox, under its policy.

```sh
pageon cdp --port 9222
# DevTools listening on ws://127.0.0.1:9222/devtools/browser/…
# WebDriver BiDi listening on ws://127.0.0.1:9222/session
```

## Puppeteer

```js
import puppeteer from 'puppeteer-core';

const browser = await puppeteer.connect({ browserURL: 'http://127.0.0.1:9222' });
const page = await browser.newPage();
await page.goto('https://example.com');
await page.waitForSelector('.late');            // virtual time: a 5 s timer resolves at once
await page.click('#buy');
await page.screenshot({ path: 'page.png', fullPage: true });

const cdp = await page.createCDPSession();
const { observation } = await cdp.send('Pageon.observe');   // the agent view, with refs
await cdp.send('Pageon.act', { action: 'click e12' });
const { data } = await cdp.send('Pageon.screenshot', { marks: true });
```

## Playwright

```js
import { chromium } from 'playwright';

const browser = await chromium.connectOverCDP('http://127.0.0.1:9222');
const page = await browser.contexts()[0].newPage();
await page.goto('https://shop.example/checkout');
await page.getByLabel('Email').fill('ann@example.com');
await page.frameLocator('#payment').getByLabel('Card').fill('4242 4242 4242 4242');
```

## What works

The CDP subset Puppeteer and Playwright use: targets and frames (same- and cross-origin),
navigation and history, evaluate and handles, selectors, locators and auto-waiting, trusted
clicks and typing, cookies, request/response, console and error events, and screenshots (PNG and
JPEG, viewport, full page, clip, elements). Single-page apps that route with the History API
send `Page.navigatedWithinDocument` (BiDi: `browsingContext.historyUpdated`), so `page.url()`,
`waitForURL` and `goBack` work as in Chrome.

Isolated worlds are real, as in Chrome: Puppeteer's and Playwright's utility worlds run in their
own JavaScript context over the same document. A page that redefines `Array.prototype.map`,
`getBoundingClientRect` or `getComputedStyle` cannot falsify what locators, `isVisible` or
`waitForSelector` compute. Objects never cross worlds.

Methods pageon lacks fail at once with what to use instead, for example request interception
(use `--origin` or a recorded session) or scripts that run before the page's own.

## Time

Pages run on a virtual clock. While the client awaits a promise, the page's time runs ahead to
the next due task, so a 5-second timer resolves immediately. A client that polls sees time pass
as the wall clock does.

## Rules of the endpoint

- Loopback only, with `Host` and `Origin` checks; `--allow-origin` admits a browser origin.
- One client at a time; the server exits when it disconnects or calls `Browser.close`.
- Every session option applies: `--origin`, `--secret`, `--guard`, `--record`, `--replay`.

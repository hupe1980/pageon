+++
title = "Screenshots and set-of-marks"
description = "Screenshots from the layout the agent acts on: viewport and full page, set-of-marks labelled with refs, crops with provenance, and masks."
weight = 22
[extra]
seo_title = "Screenshots and set-of-marks for vision models"
group = "Concepts"
nav = "Screenshots"
+++

pageon paints only when asked, from the same layout its actions use, so what a vision model
sees is exactly what a click hits. A viewport PNG takes about 2 ms.

```sh
pageon fetch https://shop.example --screenshot page.png               # the viewport, where it is scrolled to
pageon fetch https://shop.example --screenshot all.png --full-page    # below the fold too
pageon fetch https://shop.example --screenshot som.png --marks        # set-of-marks
pageon fetch https://shop.example --screenshot pay.png --crop e9      # one element
pageon fetch https://shop.example --screenshot safe.png --mask        # untrusted content masked
```

## Set-of-marks

`--marks` outlines every visible element and labels it with its ref from the `ax` view; the
legend goes to stdout (`button e7 "Log out"`). A vision model can answer `click e7`, and the
action resolves to the same element.

## Crops with provenance

`--crop e9` paints one element and says where its pixels come from: this page, or a
third-party frame and its origin. A model is told when it is looking at an ad or a widget.

## Masks

`--mask` paints over third-party frames and text a person could not read (too small, or too
little contrast): common carriers of injected instructions aimed at vision models. The notes say
what was masked.

## Secrets never reach pixels

A field filled with a `secret://` handle paints as dots, like a password field, whatever its
type.

Over MCP use `browser_screenshot {"marks": true}`; in Python `tab.screenshot(marks=True)`;
over CDP and BiDi the standard screenshot commands.

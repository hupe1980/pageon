+++
title = "Secrets"
description = "Let agents log in without seeing passwords: secret:// handles bound to origins, TOTP codes, and refusal at egress outside a secret's origin."
weight = 31
[extra]
seo_title = "Secrets: logins without exposing passwords to the model"
group = "Safety"
+++

An agent can log in without ever seeing the password. It types a handle; pageon types the value.

```sh
export PAGEON_SECRET_SHOP='correct horse battery staple'
pageon mcp --origin https://shop.example --secret shop@https://shop.example
```

```text
type e4 secret://shop
```

## How it works

- **Bound to origins.** `shop@https://shop.example` may only be typed into pages of that
  origin. Typing it into another frame or site fails with a `secret` error.
- **Resolved in the broker.** The value lives outside the page process and outside the model's
  context. The page receives it at once, as a password manager fills a field (`input` and
  `change`, no key events), so the page never holds part of it.
- **Never shown.** Views show the field as `="•••"`, screenshots paint dots, and outcomes,
  page tool results, recordings, logs and CDP and BiDi messages carry the handle instead of the
  value when the page echoes it, in every tab of the session, also after the tab that typed it
  closed. A page that transforms the value before echoing it (reverses or splits it) is not
  caught; see the
  [threat model](https://github.com/hupe1980/pageon/blob/main/THREAT_MODEL.md) (P-07).
- **Refused at egress.** If a page tries to send the secret to any origin outside its binding,
  the request is refused. The check covers the raw value and its common encodings
  (URL-encoded, form-encoded, JSON-escaped, base64) in URLs and bodies up to 1 MiB; header
  values are checked as typed. Requests that
  carry it to its own origin are labelled `secret:shop` for the [guard](@/docs/security.md#guard).

## One-time codes

```sh
export PAGEON_SECRET_OTP='JBSWY3DPEHPK3PXP'      # the TOTP seed
pageon mcp --secret otp:totp@https://shop.example
```

`type e5 secret://otp` types the current six-digit code.

## In code

```python
Browser(secrets=["shop@https://shop.example"])           # Python; value from PAGEON_SECRET_SHOP
```

```rust
let env = Secrets::new(Live::default())
    .with("shop", Secret::value(password).origins(["https://shop.example"]));
```

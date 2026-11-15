+++
title = "Secrets"
description = "Let agents log in without seeing passwords: secret:// handles bound to origins, TOTP codes, passkeys held by the broker, and refusal at egress outside a secret's origin."
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
  [threat model](https://docs.rs/pageon/latest/pageon/threat_model/) (P-07).
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

## Passkeys

```sh
pageon mcp --passkeys ~/.config/pageon/passkeys.json
```

The agent clicks a site's "Sign in with a passkey" (or "Create a passkey") button, and the
site's WebAuthn call is answered from the store: ES256 credentials, discoverable, created with
the `none` attestation.

- **Held by the broker.** Private keys never enter the page process, a recording, an outcome or
  a view. The store is a JSON file created with mode 0600; passkeys a site creates are added.
- **Bound to the site.** A credential signs only for its relying party, checked against the
  page's origin in the broker, and only after a click or key the agent sent.
- **What sites see.** A platform authenticator that has verified its user. There is no
  autofill picker (`isConditionalMediationAvailable()` is false), so sites show their button;
  with several passkeys for a site, the newest signs.

Passkeys on a person's own phone or security key are not the agent's: hand those sign-ins to a
person with a [takeover](@/docs/mcp.md#handing-over-to-a-person), and the session keeps the
signed-in state.

## In code

```python
Browser(secrets=["shop@https://shop.example"])           # Python; value from PAGEON_SECRET_SHOP
Browser(passkeys="~/.config/pageon/passkeys.json")
```

```rust
let env = Secrets::new(Live::default())
    .with("shop", Secret::value(password).origins(["https://shop.example"]));
```

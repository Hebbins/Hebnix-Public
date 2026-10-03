# Hebnix translations

English (`en`) is the source. Every other language is a translation of the same
message ids. You never need to touch Rust code to add or fix a language.

## Layout

```
locales/
  locales.toml        one row per language: code, native name, direction, status
  en/hebnix.ftl       source of truth
  de/hebnix.ftl       a translation (draft)
```

Files are [Fluent](https://projectfluent.org/) (`.ftl`). A folder can hold more
than one `.ftl` file, they are merged.

## Adding a language

1. Copy `locales/en` to `locales/<code>` (BCP-47 code, e.g. `fr`, `pt-BR`, `zh-Hans`).
2. Translate the text after each `=`. Keep the ids and the `{ $variables }`.
3. Add a row to `locales.toml`. Leave `status = "draft"` until a fluent speaker
   has reviewed it. Drafts show up as `Name (draft)` in the language picker.
4. Run `cargo test -p hebnix-app i18n`.

No code changes, the build picks the folder up automatically.

## Testing without rebuilding

- Put your files in `%AppData%\Hebnix\locales\<code>\*.ftl`. They layer over the
  built-in ones, and new folders appear in the picker.
- Force a language without touching settings or the OS language:
  `set HEBNIX_LANG=de` before starting Hebnix. This beats the saved setting.

## How the language is chosen

1. `HEBNIX_LANG` environment variable
2. Settings > Interface > Language (`language` in `config.toml`, `auto` by default)
3. The Windows display language
4. English

Anything missing in the chosen language falls back to English, then to the
message id (and is logged once), so a gap never shows an empty label.

## Writing messages

```
hello-user = Hello, { $name }!

maps-count =
    { $count ->
        [one] { $count } map
       *[other] { $count } maps
    }
```

- Plurals use a selector on `$count`. Use the plural categories of *your*
  language (`one`, `few`, `many`, `other`...), and always keep one `*[other]`
  default.
- Don't translate: ini keys (`PacketSendRate`), file names, `[Added]`-style
  server tags, product names.
- Numbers, sizes and dates are formatted by the app per language, not in `.ftl`.
- Text may be much longer than English. Hebnix sizes label columns to the
  longest label, but keep buttons short when you can.

## Checks

`cargo test -p hebnix-app i18n` fails on:

- a `.ftl` file that doesn't parse
- a message id that isn't in `en`
- different `{ $variables }` than the English message
- a plural selector without a `*[other]` default
- a language in `locales.toml` without a folder (or the other way round)
- a key used in code that `en` doesn't define

It only warns about untranslated ids (they fall back to English). To make that a
failure too: `HEBNIX_I18N_STRICT=1`. For the todo list per language:

```
cargo test -p hebnix-app i18n::tests::report -- --ignored --nocapture
```

## Notes

- Translations written by an AI are drafts, not authoritative. Only mark a
  language `reviewed` after a human fluent speaker signs it off.
- Right-to-left (Arabic, Hebrew) is not supported yet: egui has no RTL text
  shaping. `direction = "rtl"` is recorded so the UI can be mirrored later.
- CJK, Thai and Devanagari use a Windows system font as a fallback
  (`src/i18n/fonts.rs`), there is no bundled font.
- Not yet migrated strings are listed in `MIGRATION.md`.

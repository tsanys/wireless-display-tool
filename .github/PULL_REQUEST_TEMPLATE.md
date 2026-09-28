## What does this PR change?

<!-- A concise description of the change and the motivation behind it. -->

## Related issues

<!-- e.g. Closes #12, Relates to #34 -->

## Type of change

- [ ] Bug fix (`fix:`)
- [ ] New feature (`feat:`)
- [ ] Documentation (`docs:`)
- [ ] CI / build (`ci:`, `chore:`)
- [ ] Refactor / tests (`refactor:`, `test:`)

## How was it verified?

<!-- Commands you ran and what you observed. Include hardware/OS details for
     anything device- or platform-related. -->

- [ ] `cargo fmt --all -- --check`
- [ ] `cargo test --workspace` (macOS) / `cargo test -p wdt-core` (Windows)
- [ ] `cd sender-app && npm run build && npm run i18n:check` (if frontend changed)
- [ ] `cd receiver-app && ./gradlew testDebugUnitTest lint assembleDebug` (if receiver changed)
- [ ] `python3 receiver-app/scripts/check_i18n.py` (if strings changed)

## Checklist

- [ ] I updated `CHANGELOG.md` under `[Unreleased]` for user-visible changes.
- [ ] I added/updated tests where it makes sense.
- [ ] No secrets, pairing tokens, or private IPs are included.
- [ ] New UI strings are added to both languages (ID + EN).

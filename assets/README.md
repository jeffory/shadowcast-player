# Assets

## Application icon

`shadowcast.png` is the application icon used by the release workflow's
**AppImage** build (`Build AppImage (Linux)` in `.github/workflows/release.yml`).

- **Path:** `assets/shadowcast.png`
- **Format:** PNG, square
- The workflow normalizes it to 256×256 at build time (the standard AppImage
  icon size) before handing it to `linuxdeploy`; the source file here is left
  untouched, so committing a larger square master is fine.

The normalized icon's name (`shadowcast`) must match the `Icon=` key in the
generated `.desktop` entry. If `assets/shadowcast.png` is missing, the AppImage
job fails fast with a clear error.

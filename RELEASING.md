# Releasing PullDay

Every release on GitHub has a downloadable `PullDay-<version>.dmg` for macOS, `PullDay-<version>-amd64.deb` for Linux, and notes taken from [CHANGELOG.md](CHANGELOG.md). The [Release workflow](.github/workflows/release.yml) builds and publishes all three, so a release is mostly writing the notes and pushing a tag.

## Versions

Versions are `X.Y.Z`, loosely following [semantic versioning](https://semver.org):

- **Patch** (`1.0.2`): bug fixes, and new agents or sources. A new source only adds items to the menu, users can turn it off in **Settings → Sources**, and nothing they already rely on changes, so it ships as a patch (1.0.2 added Antigravity this way).
- **Minor** (`1.1.0`): other new features, such as new settings, new windows or behavior, or a new platform.
- **Major** (`2.0.0`): changes that break what users rely on, such as removing a source or changing a default.

## While you work

Every change a user would notice gets a line under `## Unreleased` at the top of `CHANGELOG.md`, in the same pull request. Write it for users: what changed for them, not how the code changed.

## Cutting a release

1. On `main`, in one commit:
   - set `CFBundleShortVersionString` in `Info.plist` to the new version;
   - set `version` in `rust/pullday-linux/Cargo.toml` to the same version, and refresh the lock with `cargo update --workspace` in `rust/`;
   - rename `## Unreleased` in `CHANGELOG.md` to `## <version> - <YYYY-MM-DD>`.
2. Push that commit, then tag it and push the tag:
   ```bash
   git tag -a v1.0.1 -m "PullDay 1.0.1"
   git push origin main v1.0.1
   ```
3. The Release workflow then:
   - stops if the tag disagrees with `Info.plist` or `rust/pullday-linux/Cargo.toml`, or if `CHANGELOG.md` on `main` has no section for the version;
   - on Ubuntu 24.04, builds `PullDay-<version>-amd64.deb`, checks it, and installs, reinstalls and removes it in a clean `ubuntu:24.04` container. If any of that fails, nothing is published;
   - runs the self-test and builds `PullDay-<version>.dmg`;
   - publishes the GitHub release with that section as the notes, install steps and the SHA-256 of the DMG and the `.deb`.
4. Check the release page, download the DMG and the `.deb`, check their SHA-256 against the notes, and open the app once.

To rebuild the release of an existing tag, for example after fixing its notes on `main`, run the Release workflow from the **Actions** tab and enter the tag. It replaces the DMG, the `.deb` and the notes. Tags from before the Linux app (v1.0.3 and older) have no `.deb`, so their rebuild replaces the DMG and the notes only.

## Signing

Release builds are ad-hoc signed, so macOS asks users to approve the app the first time (the notes explain how). Signing with a Developer ID and notarizing needs the certificate and an App Store Connect key as repository secrets, passed to `./build.sh --dmg` as `SIGN_IDENTITY` and `NOTARY_PROFILE`. That isn't set up yet.

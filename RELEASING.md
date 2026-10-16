# Releasing PullDay

Every release on GitHub has a downloadable `PullDay-<version>.dmg` and notes taken from [CHANGELOG.md](CHANGELOG.md). The [Release workflow](.github/workflows/release.yml) builds and publishes both, so a release is mostly writing the notes and pushing a tag.

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
   - rename `## Unreleased` in `CHANGELOG.md` to `## <version> - <YYYY-MM-DD>`.
2. Push that commit, then tag it and push the tag:
   ```bash
   git tag -a v1.0.1 -m "PullDay 1.0.1"
   git push origin main v1.0.1
   ```
3. The Release workflow then:
   - stops if the tag and `Info.plist` disagree, or if `CHANGELOG.md` on `main` has no section for the version;
   - runs the self-test and builds `PullDay-<version>.dmg`;
   - publishes the GitHub release with that section as the notes, install steps and the DMG's SHA-256.
4. Check the release page, download the DMG, and open the app once.

To rebuild the release of an existing tag, for example after fixing its notes on `main`, run the Release workflow from the **Actions** tab and enter the tag. It replaces the DMG and the notes.

## Signing

Release builds are ad-hoc signed, so macOS asks users to approve the app the first time (the notes explain how). Signing with a Developer ID and notarizing needs the certificate and an App Store Connect key as repository secrets, passed to `./build.sh --dmg` as `SIGN_IDENTITY` and `NOTARY_PROFILE`. That isn't set up yet.

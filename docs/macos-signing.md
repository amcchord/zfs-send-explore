# Developer ID signing and notarization

Published v0.6.1 Mac downloads are signed with a Developer ID Application
certificate and notarized by Apple. Local builds and initial CI artifacts use
ad-hoc signatures. Release CI creates a **draft**; its Mac artifacts must complete
this procedure before publication.

## Prerequisites

Use a trusted Mac with Xcode command-line tools, Python 3, a valid Developer ID
Application certificate and its private key in Keychain, and an existing
`notarytool` Keychain credential profile. Keep credentials outside the repository
and release archives. No certificate export or GitHub signing secrets are needed
for this local signing step.

```sh
security find-identity -v -p codesigning
xcrun notarytool history --keychain-profile YOUR_PROFILE
```

The signing identity and notarization credentials must belong to the intended
Apple Developer team. Never use an Apple Development certificate for release.

## Sign the exact release artifacts

Download both architecture-specific Mac artifacts from the tagged draft release:
`zfs-send-explore-macos-ARCH.zip` and `zfs-send-extract-macos-ARCH.tar.gz`.
Verify their GitHub asset digests and retain the originals. Run the checked-in
script from the matching release source:

```sh
export ZFSE_SIGN_IDENTITY='Developer ID Application: Your Name (TEAMID)'
export ZFSE_NOTARY_PROFILE='YOUR_PROFILE'
./scripts/notarize-macos.sh /path/to/ci-assets /path/to/new-output-arm64 arm64
./scripts/notarize-macos.sh /path/to/ci-assets /path/to/new-output-intel x86_64
```

Each output directory must be new. The script verifies the executable
architectures, signs the worker and CLI before the app, enables hardened runtime
with secure timestamps, and submits an archive containing all three executables.
No relaxed hardened-runtime entitlements are added.

After Apple returns `Accepted`, the script staples the app ticket, checks code
signatures/notarization and Gatekeeper, and repackages the app ZIP and CLI tarball.
It then builds a disk image containing that app, the CLI, a short installation
guide and license. The disk image is separately signed, submitted, stapled and
assessed by Gatekeeper. Rejected or unfinished submissions stop the script.

The output root contains only the three distributable archives and their
checksum manifest. `notarization-evidence/` retains Apple submission IDs, logs and
Gatekeeper results; `work/` retains the exact signed payload. Do not upload either
subdirectory as a release asset.

If a submission times out, Apple may still be processing it. Inspect the saved
submission ID with `xcrun notarytool info` or `wait`, then download its log. Do not
assume a timeout or upload success means acceptance. Resume the remaining
staple/package/assessment steps only after the saved submission is accepted.

## Release validation and publication

1. Confirm both architectures' app and disk-image submissions are `Accepted`;
   inspect their logs for unexpected issues.
2. Confirm `stapler validate` on each app and DMG, strict signature checks on all
   executables, and Gatekeeper acceptance for apps and disk images.
3. Extract the final archives into fresh directories and check signatures/tickets
   again. Mount each DMG read-only and inspect the bundled app and CLI.
4. Run the released CLI and exercise an app restore using the signed build. Check
   restored bytes against an independent known hash. The CLI and worker have new
   hardened-runtime signatures, so successful notarization alone is insufficient.
5. Upload the signed Mac ZIPs/tarballs and DMGs to the draft release. Preserve the
   existing Windows/Linux artifacts. Recompute the complete `SHA256SUMS.txt` over
   the final release assets, not just the Mac subset.
6. Run the **Verify signed Mac release** workflow with the draft release tag.
   It downloads the exact assets on native Apple Silicon and Intel runners,
   checks digests/signatures/tickets/Gatekeeper, and restores a synthetic file
   with both the signed CLI and the signed desktop worker. Both jobs must pass
   before publishing the draft. GitHub requires a token with push access to
   discover draft releases, so this manual workflow requests `contents: write`.
   It does not write to the release and checkout does not persist credentials.
   Explicit Bash execution keeps pipeline failures from being hidden by `tee`.

The disk images are the recommended app + CLI downloads. Bare command-line
executables and tarballs do not support stapled notarization tickets; the CLI
inside them is signed and notarized, but a first Gatekeeper assessment can need
Apple connectivity. Distribute the stapled DMG for offline preparation. The app
ZIP contains an app with its ticket already stapled.

Signing does not remove macOS's normal downloaded-app confirmation, override
organization policy, or extend support to older macOS versions/wrong CPU types.
The app requires macOS 13 or later, with separate Apple Silicon and Intel builds.

Apple references:
[Notarizing macOS software](https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution),
[Customizing the notarization workflow](https://developer.apple.com/documentation/security/customizing-the-notarization-workflow),
[Packaging Mac software](https://developer.apple.com/documentation/xcode/packaging-mac-software-for-distribution).

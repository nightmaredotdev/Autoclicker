# Supplementary license files

Some crates compiled into Autoclicker publish their packages without a
license file. For those, `scripts/package.ps1` takes the license text from
`packaging/licenses/<crate>-<version>/` and adds it to
`THIRD-PARTY-NOTICES.txt`. `scripts/verify-package.ps1` checks that each text
reaches the release package.

Rules:
* Only unmodified upstream license files, taken from the exact commit the
  crates.io release was published from (see the crate's
  `.cargo_vcs_info.json`). Never hand-written or approximated text.
* `SOURCE.txt` records where each file came from.
* The folder name includes the version, so upgrading the crate requires
  checking the license again instead of silently reusing an old text.

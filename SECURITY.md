# Security

## Threat model

Clementine converts files the user drops on it, and those files are often downloads: **the content
and the name of every input are untrusted.** Clementine itself parses almost nothing. It hands
inputs to ffmpeg, ImageMagick, pandoc, LibreOffice, poppler, qpdf, Ghostscript and the archive
tools. So the job is to make sure a crafted file cannot use those programs to:

- write outside the folder an output belongs in (archive traversal, links),
- read other local files into an output the user may then share (exfiltration),
- reach the network (tracking, fetching more payloads),
- run code (document macros, scripting coders),
- or be taken for an option or a different file (argument injection).

Out of scope: memory-safety bugs inside the converters (keep them updated; the Flatpak gets
updates with its runtime), resource exhaustion (a decompression bomb can fill a disk or take a
long time, and Cancel is the remedy), and other processes running as the same user.

## Mitigations

| Risk | Mitigation | Test (`src/security_tests.rs` unless noted) |
|---|---|---|
| Archive entry named `/abs` or `../x` | Entries are listed first (`unzip -Z1`, `tar -tf`, `7z l -slt`, `unrar lb`) and the archive is refused if any entry is unsafe. unzip, GNU tar and 7-Zip also strip or refuse these themselves. | `entries_climbing_out_are_refused`, `entry_names_are_checked` |
| Archive symlink pointing outside (left in the output, or followed later) | After extraction the folder is walked without following links. A link that is absolute or resolves outside is removed, and so are device and pipe nodes. tar runs with `--no-same-owner --no-same-permissions`. | `extracting_drops_links_that_point_outside`, `seven_zip_links_never_survive` |
| Archive → archive packing a linked local file (e.g. a key) into the new archive | Repacking uses `zip -y` (links stay links); tar never follows. | `repacking_never_copies_a_linked_file_in` |
| Image content choosing the decoder (an SVG named `.png`) | Every ImageMagick input is written `coder:/abs/path`, with the coder taken from the extension. | `image_content_cannot_choose_the_decoder` |
| ImageMagick scripting and file-reading coders (`msl:`, `text:`, `@list`, URL, PostScript via Ghostscript) | Bundled `data/imagemagick/policy.xml`, loaded through `MAGICK_CONFIGURE_PATH` on top of the system policy. | `magick_refuses_script_and_text_coders`, `svg_cannot_read_local_files_through_coders` |
| Disguised HLS playlist or concat list making ffmpeg fetch URLs | `-protocol_whitelist file` on every ffmpeg and ffprobe input. | `media_playlists_cannot_reach_the_network` |
| Document naming local images (`![](/home/…)`) or remote ones, embedded by pandoc | `--sandbox` when a probe shows it works. pandoc 3.1.3 cannot read its own DOCX/ODT/EPUB templates in the sandbox, so otherwise `data/pandoc/local-only.lua` drops images with a URL scheme, an absolute path, `~` or `..`. | `documents_cannot_embed_local_files` |
| Office document macros | Clementine's own LibreOffice profile is pinned to `MacroSecurityLevel=3` and `DisableMacrosExecution=true` before every run. | `the_office_profile_pins_macros_off`, `office_documents_never_run_macros` (ignored, slow) |
| File names starting with `-` or containing `:` / `[…]` | Every path handed to a program is absolute (`/…`), so it can be neither an option nor a protocol or coder prefix. The CLI makes its arguments absolute; the file-manager menus pass `--` before the files. `notify-send` gets `--` before the text it shows. | `cli::tests::notification_text_is_never_an_option` |
| Output written through a dangling symlink | Output names count any existing directory entry, links included, as taken. | `a_dangling_link_is_never_an_output_name` |
| Copy/Move following links (copying what a downloaded folder links to; `loop -> .` recursing forever), writing through a link at the destination, moving a folder into itself | Links are copied as links, new files are created with `create_new`, and moving or copying a folder into itself is refused. | `actions::tests::links_are_copied_as_links`, `…dangling_link_at_the_destination…`, `…never_moved_into_itself` |
| Nautilus script glob-expanding a file named `*` into the whole folder | `set -f` before splitting the selection. | manual |

Scratch files go in a fresh `0700` folder from `tempfile`, removed when the job ends, whether it
succeeds or fails. A failed job removes the outputs it claimed. Outputs are created by the
converters with the user's umask.

The Flatpak has no network access (`--share=network` is not granted), so even a converter bug
cannot reach the network there.

## Known gaps

- **Output name races.** An output name is chosen when the job is planned and written later.
  Another process (same user) creating that name in between could have it overwritten by tools
  that overwrite (ImageMagick, LibreOffice moves), or deleted if the job then fails. Closing this
  needs every converter to write into scratch and the result to be moved with no-replace
  semantics. Not worth it for a same-user race.
- **Local media read by playlists.** ffmpeg's HLS and concat demuxers can still read local
  *media* files (their own extension allowlist and `safe=1` rules apply). Restricting demuxers with
  `-format_whitelist` risks breaking ordinary files and was not done.
- **SVG external images.** An SVG may reference other local images by path. The policy blocks the
  dangerous coders, and librsvg refuses files outside the SVG's folder, but ImageMagick's internal
  MSVG renderer does not restrict plain image paths.
- **LibreOffice linked content.** Link updating on load was not pinned (the setting names and
  values could not be verified here). Headless conversion was not seen to run macros even with
  macro security turned off.
- **unrar** versions older than 6.12 have a known symlink traversal (CVE-2022-30333); the listing
  check refuses `..`/absolute names but cannot see link targets in a RAR listing. The
  post-extraction walk removes links leading out, but a write through such a link during
  extraction would already have happened. Keep unrar updated.
- **The Zip tool** follows symlinks in the user's own selection, as `zip -r` does. These are the
  user's own files, so this is intended.
- **Resource limits.** There are none on decompression or decoding (bombs), beyond what each
  tool's own defaults and the ImageMagick system policy give.

## Reporting

Please report vulnerabilities privately through GitHub's security advisory form on the
repository rather than in a public issue.

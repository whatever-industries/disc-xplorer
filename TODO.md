# TODO

## Formats and features

- AaruFormat (.aif) — full browsing (complex multi-codec format; currently detect-only)
- Redumper DVD/BD dumps (.sdram/.sbram) — implemented but untested (no test files available)
- STFS (Xbox 360 CON/LIVE/PIRS packages) — not implemented; no legitimate sample available (they are game saves and DLC). Unlike the other containers, a misread produces plausible-looking garbage rather than a clean failure, because the format is hashed and block-chained, so it wants a real file to write against. Reference: SabreTools.Serialization STFS.cs (MIT), and "STFS Notes.txt" in github.com/Gualdimar/Velocity
- Format conversion, as the natural companion to the batch window: RVZ/WIA/GCZ/WBFS to ISO (GameCube, Wii), CSO/ZSO to and from ISO (PSP, PS2), CHD to CUE/BIN. Every one of these has a working reader already, so the work is a writer plus a job kind, not new format research. Deliberately not more encryption: the only targets left (3DS NCCH, Vita) need console-unique keys, unlike the published, disc-specific 3DO and PS3 ones.
- Verify against Redump/No-Intro DATs, as a Tools entry: hash a folder of images and report verified / bad / unknown. blake3 and md-5 are already dependencies; CRC32 and SHA-1 would be small additions. The part worth building rather than copying is hashing the *logical contents* as well as the file — a CHD, an RVZ and an ISO of one disc are byte-different but hold identical data, and since all three are readable here we can match a compressed dump against a DAT entry that only lists the ISO, which existing verifiers cannot.
- Batch Extract, as a Tools entry: point at a folder and extract every image's contents into per-disc folders. Reuses the extraction paths, the CD-XA and gap-handling settings, and the whole pre-flight, job queue and log built for Batch Convert, so it is close to a planner plus a job kind.
- Batch audio rip, as a Tools entry: rip whole CD collections to WAV/FLAC/MP3 using the existing EAC-style gap handling. The natural follow-on from the in-app player; the reporter on issue #6 uses it daily and collection-scale ripping is the obvious next ask. Single-disc ripping now falls out of "Extract All Contents", so this is the batch case only.
- CD-TEXT from a drive on Linux and Windows. macOS reads it via `drutil cdtext`, and cue sheets are handled, but the other two platforms need a SCSI READ TOC/PMA/ATIP with format 5. The reply is the same 18-byte pack stream `cdtext::parse_packs` already decodes, so the work is only the ioctl wrapper — SG_IO on Linux, IOCTL_SCSI_PASS_THROUGH_DIRECT on Windows — plus the `libc` and `windows-sys` dependencies neither of which the project has yet. Left out deliberately: it is unsafe code that cannot be exercised from macOS. Worth doing when a user on those platforms asks, or when a disc carrying CD-TEXT turns up to test against. Note the `drutil` output parser is itself an unverified guess, since its format is undocumented and no disc here has CD-TEXT; if it comes back empty on a disc that has some, the raw `drutil cdtext` output is all that is needed to fix it.
- SBI subchannel files (.sbi) — not read. They carry the LibCrypt subchannel data a PS1 dump needs for its copy protection to check out, patched over the Q subchannel at the sectors the file lists. Sibling of the ProtectCD/DPM work: same idea of protection living outside the file data, different mechanism and console. Format is simple (header, then per-entry MSF plus subchannel bytes), and 1583 real files for PS1 and PC are at archive.org/details/video_game_keys_and_sbi, so this one is testable unlike most of the list.

## Housekeeping

- No LICENSE file in the repo, although the "View licenses" modal in `App.tsx` tells users the app is GPL v3. Whatever the intent, the repo and the app should agree.

## App Store build

Blockers found auditing whether the app could be sold on the Mac App Store. None
of these affect the direct-download build, which stays as it is. Taken together
they describe a separate, smaller build target rather than a change to the main
one. Not legal advice — worth a real IP lawyer before selling, especially the
last item.

- **The app declares itself GPL v3** (the "View licenses" modal in `App.tsx`), and there is no LICENSE file in the repo to match. GPL is incompatible with App Store terms, which add restrictions GPL §7 forbids — this is why VLC was pulled in 2011. Selling is not the problem; GPL permits charging. Relicensing our own code is possible only once nothing we depend on forces GPL, i.e. after the two items below.
- **redumper is GPL-3.0 and we bundle it** as a sidecar (`externalBin` in `tauri.conf.json`). Running it as a separate process arguably does not make our app a derivative work, but *distributing* the binary through the App Store is itself the violation. An App Store build must not ship it — either drop dumping or locate a user-installed copy.
- **LAME is LGPL-3.0 and statically linked** (`mp3lame-encoder`). LGPL wants users to be able to relink against a modified LAME, which App Store signing prevents. An App Store build has to drop MP3 export. WAV and FLAC are unaffected — libFLAC is BSD-3-Clause.
- **Sandboxing removes disc dumping anyway.** App Store apps must be sandboxed and raw SCSI/optical access is not available there, so "Dump Disc from Drive" cannot work regardless of the redumper licensing. The two constraints happen to point the same way.
- **App Review Guideline 5.2 (Intellectual Property) is the real risk.** An app that decrypts PS3 and Wii U discs with title keys and extracts commercial game content is a plausible rejection on IP grounds, and that is a reviewer judgement no amount of paperwork settles. Likely the deciding factor, above all the licensing work.

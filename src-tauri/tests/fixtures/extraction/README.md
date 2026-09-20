These tiny ZIP and USTAR archives were generated independently with Python's
standard `zipfile` and `tarfile` modules. Each archive contains two files, with
payloads `first source file` and `second source file` respectively:

| Fixture | First path | Second path |
| --- | --- | --- |
| collision | `track?.txt` | `track_.txt` |
| folders | `a?/one.txt` | `a*/two.txt` |
| file-directory | `a?` | `a_/child.txt` |
| control | `first.txt` | `dir/second.txt` |

ZIP entries use stored compression and the timestamp 2000-01-01 00:00:00.
TAR entries use USTAR headers, timestamp 0, and sizes matching the payloads.
All content is synthetic; no disc images or third-party data are included.

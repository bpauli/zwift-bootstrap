# Update protocol

This document describes the files that `zwift-bootstrap` reads from the Zwift CDN. All facts come from public HTTP responses. No request needs a login.

## Base URL

```text
https://cdn.zwift.com/gameassets/Zwift_Updates_Root/
```

## Version file

`GET {base}Zwift_ver_cur.xml` returns one empty XML element:

```xml
<Zwift version="1.0.167050" sversion="1.123.0 (167050)" gbranch="..." gcommit="..." GAME_URL="https://us-or-rly101.zwift.com" manifest="Zwift_1.0.167050_ce7420cb_manifest.xml" manifest_checksum="667523848" ver_cur_checksum="1134193414"/>
```

| Attribute | Use |
| --- | --- |
| `version` | Game version, `major.minor.build`. |
| `manifest` | File name of the manifest, relative to the base URL. |
| `manifest_checksum` | CRC-32 of the manifest file. `-1` means "not provided". |
| `mandatory_version` | Optional. Printed by `check`. |

The tool ignores the other attributes.

## Manifest

`GET {base}{manifest}` returns the list of game files. The response can come with `Content-Encoding: gzip`.

```xml
<Zwift version="1.0.167050" folder="Zwift_1.0.167050_ce7420cb">
    <files>
        <file>
            <path>data\Worlds\world9\map04.ztx</path>
            <length>60</length>
            <modified>2026-10-01 17:32:38.235</modified>
            <checksum>351459945</checksum>
        </file>
    </files>
</Zwift>
```

| Item | Use |
| --- | --- |
| `folder` | Server folder that holds the files of this version. |
| `path` | Path relative to the install directory, with `\` separators. |
| `length` | File size in bytes. |
| `modified` | File time stamp. The tool treats it as UTC and sets the file time from it. |
| `checksum` | CRC-32 of the file. `-1` means "not provided". |

A current manifest lists about 19800 files and 4.8 GiB.

## Game files

`GET {base}{folder}/{path}` with each `\` in `path` changed to `/`. The server sends the raw file bytes. A path that keeps `\` as `%5C` gets HTTP 403.

## Checksums

All checksums are standard CRC-32 (IEEE, the zlib `crc32`), written as a signed 32-bit decimal. To check one by hand:

```sh
python3 -c 'import sys,zlib; v=zlib.crc32(open(sys.argv[1],"rb").read()); print(v-(1<<32) if v>=1<<31 else v)' FILE
```

## Install directory

Game files go straight into the install directory at their manifest paths. The tool also keeps a copy of the manifest and of `Zwift_ver_cur.xml` there, and its own state in `.zwift-bootstrap/`.

## Game start

`launch` runs this command in the install directory:

```text
wine ZwiftApp.exe --launcher_version=1.2.0 --token=
```

With an empty token, the game shows its own login screen.

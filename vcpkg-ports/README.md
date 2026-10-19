# vcpkg port overrides

Copies of vcpkg ports that nicedelay needs changed, used instead of vcpkg's own
(`--overlay-ports` in `scripts/build-ffmpeg.sh`). Delete one once vcpkg has the fix.

- **amd-amf**: AMD repackaged `AMF-headers-v1.5.3.tar.gz` on 2026-10-16, so it no longer
  matches vcpkg's SHA-512. The headers inside are identical to the `v1.5.3` tag of
  https://github.com/GPUOpen-LibrariesAndSDKs/AMF (`amf/public/include`); only the
  checksum is updated here.

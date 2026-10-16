#!/usr/bin/env bash
# Stream a test pattern (with a visible running counter and a beep every second) to the
# local ingest, like OBS would. Usage: scripts/fake-obs.sh [seconds] [url]
DURATION=${1:-600}
URL=${2:-rtmp://127.0.0.1:1935/live/nicedelay}
exec ffmpeg -hide_banner -loglevel error -re \
  -f lavfi -i "testsrc=size=1920x1080:rate=30" \
  -f lavfi -i "sine=frequency=880:beep_factor=4:sample_rate=48000" \
  -t "$DURATION" \
  -c:v libx264 -preset veryfast -tune zerolatency -b:v 4500k -g 60 -pix_fmt yuv420p \
  -c:a aac -b:a 128k -ar 48000 -ac 2 \
  -f flv "$URL"

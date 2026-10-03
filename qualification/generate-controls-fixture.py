#!/usr/bin/env python3
"""Generate silent video with alternate audio/subtitle tracks for GTK checks."""
import json
from pathlib import Path
import subprocess
import sys

out = Path(sys.argv[1]).resolve()
out.mkdir(parents=True, exist_ok=True)
for language, text in [("en", "English"), ("es", "Spanish")]:
    (out / f"{language}.srt").write_text("\n".join(
        f"{i + 1}\n00:00:{i:02d},000 --> 00:00:{i + 1:02d},000\n{text} caption check\n"
        for i in range(12)))
media = out / "controls-silent.mkv"
subprocess.run([
    "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
    "-f", "lavfi", "-i", "color=c=blue:size=640x360:rate=25:duration=12",
    "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo",
    "-f", "lavfi", "-i", "anullsrc=r=48000:cl=stereo",
    "-i", str(out / "en.srt"), "-i", str(out / "es.srt"),
    "-map", "0:v", "-map", "1:a", "-map", "2:a", "-map", "3:s", "-map", "4:s",
    "-t", "12", "-c:v", "libx264", "-preset", "ultrafast", "-g", "25",
    "-c:a", "aac", "-c:s", "srt", "-metadata:s:a:0", "language=eng",
    "-metadata:s:a:1", "language=spa", "-metadata:s:s:0", "language=eng",
    "-metadata:s:s:1", "language=spa", str(media),
], check=True)
(out / "cases.json").write_text(json.dumps([{
    "alias": "fixture-caption-pixels", "payload": {
        "uri": media.as_uri(), "x": 0, "y": 0, "width": 640, "height": 480,
        "autoplay": True, "muted": True, "volume": 0,
    },
}]))
print("Silent native controls fixture generated")

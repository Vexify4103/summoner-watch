"""Turns demo_frames snapshots into a GIF (and MP4) on a Stream Deck + mockup.

Usage (from the project folder):
    cargo build --release --bins --examples
    cargo run --release --example demo_frames -- target/demo
    python -I tools/render_demo.py target/demo marketing/demo

Needs Microsoft Edge (headless screenshots) and ffmpeg on PATH.
"""
import html
import json
import pathlib
import subprocess
import sys

EDGE = r"C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe"
W, H = 1280, 720
ROOT = pathlib.Path(__file__).resolve().parent.parent

PAGE = """<!doctype html><html><head><meta charset="utf-8">
<link rel="stylesheet" href="{css}">
<style>
  html, body {{ width: {w}px; height: {h}px; }}
  .brand {{ position: absolute; left: 44px; top: 34px; display: flex; align-items: center; gap: 18px; }}
  .brand img {{ width: 72px; height: 72px; border-radius: 16px; }}
  .brand b {{ font-size: 40px; font-weight: 800; }}
  .wrap {{ position: absolute; left: 50%; top: 128px; width: 560px; transform: translateX(-50%); }}
  .wrap .device {{ padding: 26px; border-radius: 34px; }}
  .wrap .keys {{ grid-template-columns: repeat(4, 1fr); gap: 14px; }}
  .wrap .key {{ aspect-ratio: 1; position: relative; }}
  .key.pressed {{ box-shadow: 0 0 0 5px var(--gold), 0 0 34px 8px rgba(232, 179, 58, 0.7); transform: scale(0.94); }}
  .wrap .strip {{ margin-top: 18px; }}
  .wrap .dials {{ margin-top: 20px; }}
  .wrap .dial {{ width: 70px; height: 70px; }}
  .cap {{ position: absolute; left: 0; right: 0; bottom: 38px; text-align: center; font-size: 38px; font-weight: 700;
          text-shadow: 0 4px 18px rgba(0, 0, 0, 0.6); }}
  .seg.empty .name {{ color: #6e7681; }}
</style></head><body>
  <div class="brand"><img src="{icon}"><b>Summoner <span class="gold">Watch</span></b></div>
  <div class="wrap"><div class="device">
    <div class="keys">{keys}</div>
    <div class="strip">{strip}</div>
    <div class="dials"><div class="dial"></div><div class="dial"></div><div class="dial"></div><div class="dial"></div></div>
  </div></div>
  <div class="cap">{caption}</div>
  <script>
    document.querySelectorAll(".seg").forEach(function (s) {{
      s.querySelector(".in").style.transform = "scale(" + s.clientWidth / 200 + ")";
    }});
  </script>
</body></html>"""


def key_html(frame: pathlib.Path, name: str, pressed: str) -> str:
    cls = "key pressed" if name == pressed else "key"
    svg = frame / f"{name}.svg"
    if not svg.exists():
        return '<div class="key blank"></div>'
    return f'<div class="{cls}"><img src="{svg.name}"></div>'


def strip_html(dials: dict) -> str:
    out = []
    for d in ("d0", "d1", "d2", "d3"):
        fb = dials.get(d, {})
        empty = fb.get("time", "") == ""
        bar = fb.get("bar", {}).get("value", 0)
        out.append(
            f'<div class="seg{" empty" if empty else ""}"><div class="in">'
            f'<img class="champ" src="{fb.get("champ", "")}">'
            f'<img class="spell" src="{fb.get("spell", "")}">'
            f'<div class="name">{html.escape(fb.get("name", ""))}</div>'
            f'<div class="time">{html.escape(fb.get("time", ""))}</div>'
            f'<div class="bar"><i style="width:{bar}%"></i></div></div></div>'
        )
    return "".join(out)


def main() -> None:
    src = pathlib.Path(sys.argv[1]).resolve()
    out = pathlib.Path(sys.argv[2]).resolve()
    out.parent.mkdir(parents=True, exist_ok=True)
    shots = src / "shots"
    shots.mkdir(exist_ok=True)
    css = (ROOT / "marketing" / "style.css").as_uri()
    icon = (ROOT / "assets" / "icon.png").as_uri()

    concat = []
    frames = sorted(p for p in src.iterdir() if p.is_dir() and p.name.isdigit())
    for f in frames:
        meta = json.loads((f / "meta.json").read_text(encoding="utf-8"))
        dials = json.loads((f / "dials.json").read_text(encoding="utf-8"))
        keys = "".join(key_html(f, k, meta["pressed"]) for k in ("k0", "k1", "k2", "k3", "k4", "insight", "reset", "-"))
        page = PAGE.format(css=css, icon=icon, w=W, h=H, keys=keys, strip=strip_html(dials),
                           caption=html.escape(meta["caption"]))
        (f / "frame.html").write_text(page, encoding="utf-8")
        png = shots / f"{f.name}.png"
        subprocess.run([EDGE, "--headless=new", "--disable-gpu", "--hide-scrollbars", "--force-device-scale-factor=1",
                        f"--window-size={W},{H}", "--virtual-time-budget=4000",
                        f"--user-data-dir={src / 'edge-profile'}", f"--screenshot={png}", (f / "frame.html").as_uri()],
                       check=True, capture_output=True)
        concat.append(f"file '{png.as_posix()}'\nduration {meta['hold_ms'] / 1000:.3f}\n")
    # The concat demuxer needs the last file repeated so its duration counts.
    concat.append(f"file '{(shots / frames[-1].name).as_posix()}.png'\n")
    (src / "concat.txt").write_text("".join(concat), encoding="utf-8")

    common = ["ffmpeg", "-y", "-loglevel", "error", "-f", "concat", "-safe", "0", "-i", str(src / "concat.txt")]
    gif_filter = ("fps=12,scale=900:-1:flags=lanczos,split[a][b];"
                  "[a]palettegen=max_colors=192:stats_mode=diff[p];[b][p]paletteuse=dither=sierra2_4a")
    subprocess.run(common + ["-vf", gif_filter, "-loop", "0", f"{out}.gif"], check=True)
    subprocess.run(common + ["-vf", "fps=30,format=yuv420p", "-c:v", "libx264", "-crf", "20",
                             "-movflags", "+faststart", f"{out}.mp4"], check=True)
    for ext in ("gif", "mp4"):
        p = pathlib.Path(f"{out}.{ext}")
        print(f"{p.name}: {p.stat().st_size / 1024:.0f} KB")


if __name__ == "__main__":
    main()

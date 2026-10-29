#!/usr/bin/env python3
"""Download screenshots of free/open-source games from Wikimedia Commons as extra training backgrounds (local use only, not shipped).
  scripts/fetch_game_backgrounds.py OUT_DIR [per_query]
Each picture keeps its Commons file name; licences are the games' own (GPL / CC), see the file pages. Standard library only."""
import json, os, sys, time, urllib.parse, urllib.request
UA = {"User-Agent": "openps-research/0.1 (training backgrounds; local use)"}
QUERIES = ["SuperTuxKart screenshot", "0 A.D. screenshot", "Xonotic screenshot", "OpenTTD screenshot", "Battle for Wesnoth screenshot", "Veloren screenshot",
           "Warzone 2100 screenshot", "Minetest screenshot", "OpenArena screenshot", "Cube 2 Sauerbraten screenshot", "FlightGear screenshot", "TORCS screenshot",
           "Stunt Rally screenshot", "Blender open movie game screenshot", "Godot game screenshot", "racing game screenshot GPL", "first-person shooter screenshot GPL",
           "Tremulous screenshot", "Unvanquished screenshot", "Hedgewars screenshot", "Widelands screenshot", "Armagetron Advanced screenshot", "Frozen Bubble screenshot",
           "Teeworlds screenshot", "OpenRA screenshot", "Freeciv screenshot", "Flare RPG screenshot", "Mindustry screenshot", "Luanti screenshot", "Yo Frankie screenshot"]
def get(url):
    return urllib.request.urlopen(urllib.request.Request(url, headers=UA), timeout=40).read()
def main(out, per=12):
    os.makedirs(out, exist_ok=True)
    n = 0
    for q in QUERIES:
        api = "https://commons.wikimedia.org/w/api.php?" + urllib.parse.urlencode({"action": "query", "format": "json", "generator": "search", "gsrnamespace": 6,
              "gsrsearch": q, "gsrlimit": per, "prop": "imageinfo", "iiprop": "url|mime", "iiurlwidth": 960})
        try:
            pages = json.loads(get(api)).get("query", {}).get("pages", {})
        except Exception as e:
            print("query failed", q, e); continue
        for p in pages.values():
            ii = (p.get("imageinfo") or [{}])[0]
            if not ii.get("mime", "").startswith("image/") or "svg" in ii.get("mime", ""):
                continue
            url = ii.get("thumburl") or ii.get("url")
            name = "".join(c if c.isalnum() else "_" for c in p["title"][5:])[:80] + ".jpg"
            path = os.path.join(out, name)
            if os.path.exists(path):
                continue
            try:
                open(path, "wb").write(get(url)); n += 1
            except Exception as e:
                print("skip", name, e)
            time.sleep(0.3)
        print(q, "->", n, "total", flush=True)
if __name__ == "__main__":
    main(sys.argv[1], int(sys.argv[2]) if len(sys.argv) > 2 else 12)

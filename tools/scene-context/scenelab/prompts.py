"""The sentences the zero-shot reading compares a frame to (MobileCLIP text tower): one per class, and one per view.

Their embeddings ship in scene-probe.json, so changing a sentence needs a new export (`train.sh --reexport`)."""
import hashlib
import json

PROMPTS = {
    "on_foot": "a third-person view of a character walking on foot in a video game",
    "land_vehicle": "a car or motorcycle driving on a road in a video game",
    "air_vehicle": "a helicopter or airplane flying in the sky in a video game",
    "water_vehicle": "a boat or jet ski on the water in a video game",
    "menu_or_cutscene": "a menu, map or loading screen in a video game",
}
VIEW_PROMPTS = [
    "a first-person view in a video game, seen through the character's eyes or from inside a vehicle cockpit",
    "a third-person view in a video game, seen from behind the character or vehicle",
]


def fingerprint():
    """Changes with any sentence: the probe file remembers it to know whether its text embeddings are still current."""
    return hashlib.sha1(json.dumps([PROMPTS, VIEW_PROMPTS], sort_keys=True).encode()).hexdigest()[:12]

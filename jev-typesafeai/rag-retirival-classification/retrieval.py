import json
from pathlib import Path

from cooksafe import JsonCache
from openai import OpenAI

# setup ============================================================

EMBED_MODEL = "qwen3-embedding:0.6B"
EMBED_BASE_URL = "http://192.168.1.25:11434/v1"  # local ollama, OpenAI-compatible API

json_cache = JsonCache(Path("json_cache.json"))
embedder = OpenAI(base_url=EMBED_BASE_URL, api_key="ollama")

# load docs corpus ============================================================

PASSAGES = json.loads(Path("corpus.json").read_text(encoding="utf-8"))
BY_ID = {p["id"]: p for p in PASSAGES}


# embed and retrieve ============================================================

@json_cache
def embed(texts: tuple[str, ...]) -> list[list[float]]:
    """One call for many texts; the tuple argument keeps the cache key small and hashable."""
    response = embedder.embeddings.create(
        model=EMBED_MODEL, input=list(texts)
    )
    return [item.embedding for item in response.data]


def cosine(a: list[float], b: list[float]) -> float:
    dot = sum(x * y for x, y in zip(a, b))
    return dot / ((sum(x * x for x in a) ** 0.5) * (sum(y * y for y in b) ** 0.5))


PASSAGE_VECTORS = dict(
    zip(
        [p["id"] for p in PASSAGES],
        embed(tuple(f"{p['title']}\n\n{p['text']}" for p in PASSAGES)),
    )
)


def retrieve(query: str, k: int) -> list[dict]:
    vector = embed((query,))[0]
    scored = [(cosine(vector, PASSAGE_VECTORS[p["id"]]), p["id"]) for p in PASSAGES]
    scored.sort(
        key=lambda pair: (-pair[0], pair[1])
    )  # id breaks ties, so replays match
    return [dict(BY_ID[pid], similarity=round(score, 4)) for score, pid in scored[:k]]

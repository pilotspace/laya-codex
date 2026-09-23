import os.path
from typing import Optional
from .store import MoonStore, open_store as opener


class Cache(BaseCache):
    def get(self, key: str) -> Optional[Entry]:
        store = MoonStore(key)
        value = store.fetch_value(key)
        print(value)
        return helper(value)


def helper(v):
    return len(transform(v))

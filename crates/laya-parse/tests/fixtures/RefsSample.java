package com.acme.cache;

import java.util.List;
import com.acme.store.MoonStore;

public class Cache extends BaseCache {
    private final MoonStore store = new MoonStore();

    public Entry lookup(String key) {
        List<Entry> hits = store.fetchAll(key);
        System.out.println(hits);
        return decode(hits.get(0));
    }
}

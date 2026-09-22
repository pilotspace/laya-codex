package com.example;

import java.util.HashMap;
import java.util.Map;

/** A simple store. */
public class Sample {
    private final Map<String, String> map = new HashMap<>();

    public Sample() {
        map.put("a", "b");
    }

    /** Gets a value. */
    public String get(String key) {
        String v = map.get(key);
        if (v == null) {
            return "";
        }
        return v;
    }

    @Override
    public String toString() {
        return map.toString();
    }
}

interface Keyed {
    String key();
}

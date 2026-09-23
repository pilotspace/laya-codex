package com.example

import kotlin.math.sqrt

const val MAX_KEYS = 1024

typealias Key = String

/** A key store. */
class KeyStore(private val map: MutableMap<Key, String>) {
    fun get(key: Key): String {
        return map[key] ?: ""
    }

    fun set(key: Key, value: String) {
        map[key] = value
    }

    companion object {
        fun empty(): KeyStore = KeyStore(mutableMapOf())
    }
}

object Registry {
    val stores = mutableListOf<KeyStore>()
}

fun norm(x: Double, y: Double): Double = sqrt(x * x + y * y)

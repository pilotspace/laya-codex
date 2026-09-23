import Foundation

let maxKeys = 1024

/// Something with a key.
protocol Keyed {
    func key() -> String
}

/// A key store.
class KeyStore {
    private var map: [String: String] = [:]

    init() {
        map["a"] = "b"
    }

    func get(_ key: String) -> String {
        return map[key] ?? ""
    }

    func set(_ key: String, _ value: String) {
        map[key] = value
    }
}

struct Point {
    var x: Double
    var y: Double
}

func norm(_ p: Point) -> Double {
    return (p.x * p.x + p.y * p.y).squareRoot()
}

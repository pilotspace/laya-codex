const fs = require("fs");

const RETRIES = 3;

// Loads a config file.
function loadConfig(path) {
  const raw = fs.readFileSync(path, "utf8");
  return JSON.parse(raw);
}

class Cache {
  constructor() {
    this.map = new Map();
  }

  get(key) {
    return this.map.get(key);
  }

  set(key, value) {
    this.map.set(key, value);
    return this;
  }
}

const handler = async (req) => {
  const cfg = loadConfig(req.path);
  return cfg;
};

module.exports = { loadConfig, Cache, handler };

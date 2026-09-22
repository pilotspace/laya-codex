require "json"

# Wraps configuration.
module Config
  DEFAULTS = { retries: 3 }.freeze

  # Loads settings.
  class Loader
    def initialize(path)
      @path = path
    end

    def load
      raw = File.read(@path)
      JSON.parse(raw)
    end

    def self.default
      new("config.json")
    end
  end
end

def helper(x)
  x * 2
end

using System;
using System.Collections.Generic;

namespace Example.Store
{
    /// <summary>A keyed store.</summary>
    public class KeyStore
    {
        private readonly Dictionary<string, string> _map = new();

        public int Count => _map.Count;

        public KeyStore()
        {
            _map["a"] = "b";
        }

        public string Get(string key)
        {
            return _map.TryGetValue(key, out var v) ? v : "";
        }
    }

    public interface IKeyed
    {
        string Key();
    }
}

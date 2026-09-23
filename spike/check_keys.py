"""Check that two Laya model dirs have identical weight keys/shapes (so the Rust port loads both)."""
import sys

from safetensors import safe_open

a = safe_open(sys.argv[1] + "/model.safetensors", "np")
b = safe_open(sys.argv[2] + "/model.safetensors", "np")
ka, kb = set(a.keys()), set(b.keys())
shapes_equal = all(a.get_slice(k).get_shape() == b.get_slice(k).get_shape() for k in ka & kb)
print("same keys:", ka == kb, "n=%d" % len(ka), "same shapes:", shapes_equal,
      "dtype:", b.get_slice(sorted(kb)[0]).get_dtype())

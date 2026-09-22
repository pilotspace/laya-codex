"""Module doc."""
import os


class Foo:
    """A class."""

    def bar(self, x):
        # comment for bar
        y = x + 1
        return y

    @staticmethod
    def baz():
        return os.getcwd()

    def qux(self):
        return 1


def top_level(a, b):
    return a + b


CONSTANT = 3

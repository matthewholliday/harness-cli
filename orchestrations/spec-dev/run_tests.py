#!/usr/bin/env python3
import sys
import unittest

if __name__ == "__main__":
    loader = unittest.TestLoader()
    suite = loader.discover("tests")
    runner = unittest.TextTestRunner(verbosity=2)
    sys.exit(0 if runner.run(suite).wasSuccessful() else 1)

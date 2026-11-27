import io
import unittest

from frames import summarize


def capture(lines):
    return io.StringIO("Application,ProcessID,SwapChainAddress,MsBetweenPresents\n" + lines)


class FrameTests(unittest.TestCase):
    def test_distribution(self):
        rows = ["launcher.exe,1,2,900", "victory.exe,2,3,0"]
        rows.extend(f"victory.exe,2,3,{value}" for value in range(1, 21))
        result = summarize(capture("\n".join(rows)))
        self.assertEqual(result["intervals"], 20)
        self.assertEqual(result["median_ms"], 10.5)
        self.assertEqual(result["p95_ms"], 19)
        self.assertAlmostEqual(result["fps"], 1000/10.5)

    def test_mixed_chains(self):
        with self.assertRaises(ValueError):
            summarize(capture("victory.exe,2,3,12\nvictory.exe,2,4,13"))

    def test_invalid_interval(self):
        with self.assertRaises(ValueError):
            summarize(capture("victory.exe,2,3,12\nvictory.exe,2,3,nan"))

    def test_missing_frames(self):
        with self.assertRaises(ValueError):
            summarize(capture("launcher.exe,1,2,20"))

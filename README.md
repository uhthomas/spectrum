# Spectrum

Basic implementation of an FFT audio spectrum in browser demonstrating fast and good looking visuals similar to that of pre-rendered videos are possible in real-time and in-browser.

To use either provide a youtube/soundcloud link in the hash such as https://6f.io/spectrum#https://youtube.com/watch?v=<id> or drag and drop a local media file onto the web page.

## Support

Works in the latest versions of Chrome and Edge (webkit). Firefox has a [bug](https://bugzilla.mozilla.org/show_bug.cgi?id=1579957)
where `clip-path` does not respect `backdrop-filter` and so does not display
correctly.

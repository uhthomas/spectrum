const p = document.createElementNS("http://www.w3.org/2000/svg", "path");
p.setAttribute("stroke", "white");
p.setAttribute("stroke-width", "2");
p.setAttribute("fill", "transparent");

const clipPath = document.createElementNS(
  "http://www.w3.org/2000/svg",
  "clipPath"
);
clipPath.setAttribute("id", "clip");
clipPath.appendChild(p);

const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
svg.appendChild(clipPath);

document.body.appendChild(svg);

const audioctx = new AudioContext();

const analyser = audioctx.createAnalyser();
analyser.fftSize =
  {
    41000: 4 << 10,
    48000: 4 << 10,
    96000: 8 << 10,
    192000: 16 << 10,
    384000: 32 << 10,
  }[audioctx.sampleRate] || 4 << 10;
analyser.smoothingTimeConstant = 0.67;
analyser.connect(audioctx.destination);

const src = audioctx.createMediaElementSource(media);
src.connect(analyser);

const min = -66;
const max = 12;
const spacing = 40;
(draw = () => {
  requestAnimationFrame(draw);

  const rect = media.getBoundingClientRect();

  // media aspect ratio, rect aspect ratio
  const r = media.videoWidth / media.videoHeight;
  const nr = rect.width / rect.height;

  const contentWidth = nr > r ? rect.height * r : rect.width;
  const contentHeight = nr < r ? rect.width / r : rect.height;

  const x = Math.round((rect.width - contentWidth) / 2);
  const y = Math.round((rect.height - contentHeight) / 2);

  filter.style.width = contentWidth + "px";
  filter.style.height = contentHeight + "px";
  filter.style.transform = `matrix3d(
		1, 0, 0, 0,
		0, 1, 0, 0,
		0, 0, 1, 0,
		${x}, ${y}, 0, 1
	)`;

  const scale = 1 / devicePixelRatio;

  // add 2 to compensate for start and end points.
  let arr = new Float32Array(Math.round((scale * contentWidth) / spacing + 2));
  analyser.getFloatFrequencyData(arr);

  if (arr.some((n) => !isFinite(n))) {
    return;
  }

  arr = arr.map((v) =>
    Math.max(
      0,
      contentHeight - (((v - min) * (max - min)) / 8) * devicePixelRatio
    )
  );

  let d = "";
  for (let i = 0; i < arr.length - 1; i++) {
    const x1 = i * spacing * devicePixelRatio;
    const x2 = (i + 1) * spacing * devicePixelRatio;
    const x = (x1 + x2) / 2;

    const y1 = arr[i];
    const y2 = arr[i + 1];
    const y = (y1 + y2) / 2;

    d += !i ? `M${x1} ${y1} Q${x1} ${y1} ${x} ${y} T` : `${x} ${y} `;
  }
  d += `L${
    arr.length * spacing * devicePixelRatio
  } ${contentHeight} L0 ${contentHeight} L0 ${arr[0]} Z`;

  p.setAttribute("d", d);
})();

const resumeAudioContext = () => {
  if (audioctx.state !== "running") {
    audioctx.resume();
  }
};

window.addEventListener("click", resumeAudioContext, {
  once: true,
});

const preventDefault = (e) => {
  e.preventDefault();
  if (e.dataTransfer) {
    e.dataTransfer.dropEffect = "copy";
  }
};

const load = (files) => {
  if (!files.length) {
    return;
  }

  URL.revokeObjectURL(media.src);
  media.src = URL.createObjectURL(files[0]);
  media.play();
};

window.addEventListener("dragover", preventDefault);
window.addEventListener("dragenter", preventDefault);
window.addEventListener("dragend", preventDefault);
window.addEventListener("dragleave", preventDefault);
window.addEventListener("drop", (e) => {
  e.preventDefault();
  load(e.dataTransfer.files);
});

document.addEventListener("keydown", (e) => {
  if (e.key !== "l") {
    return;
  }

  e.preventDefault();
  selectfile.click();
});

selectfile.addEventListener("change", (e) => load(e.target.files));

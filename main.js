(() => {
	const actx = new AudioContext();
	const src = actx.createMediaElementSource(media);
	const analyser = actx.createAnalyser();
	analyser.fftSize = {
		41000: 4 << 10,
		48000: 4 << 10,
		96000: 8 << 10,
		192000: 16 << 10
	}[actx.sampleRate] || 4 << 10;
	analyser.smoothingTimeConstant = .67;
	src.connect(analyser);
	analyser.connect(actx.destination);

	const ctx = canvas.getContext('2d');
	const min = -66;
	const max = 12;
	const spacing = 40;
	
	(draw = () => {
		requestAnimationFrame(draw);

		const w = innerWidth * devicePixelRatio;
		const h = innerHeight * devicePixelRatio;

		const r = media.videoWidth / media.videoHeight;
		const nr = w / h;

		canvas.width = (nr > r ? h * r : w) + .5 | 0;
		canvas.height = (nr < r ? w / r : h) + .5 | 0;

		const x = (w - canvas.width) / 2 + .5 | 0;
		const y = (h - canvas.height) / 2 + .5 | 0;

		canvas.style.transform = 'translate3d(' + x + 'px, ' + y + 'px, 0)';

		var arr = new Float32Array((4 * (1 / devicePixelRatio) * canvas.width / spacing + 2) | 0);
		analyser.getFloatFrequencyData(arr);
		ctx.clearRect(0, 0, canvas.width, canvas.height);

		arr = arr.map(v => canvas.height - ((v - min) * (max - min) / 8) * devicePixelRatio);

		ctx.beginPath();
		ctx.moveTo(0, arr[0]);
		var i = 1;
		for (; i < arr.length - 1; i++) {
			const x = i * spacing * devicePixelRatio;
			const x2 = (i + 1) * spacing * devicePixelRatio;
			const y = arr[i];
			const y2 = arr[i + 1];
			const xc = (x + x2) / 2;
			const yc = (y + y2) / 2;
			ctx.quadraticCurveTo(x, y, xc, yc);
		}
		ctx.quadraticCurveTo(i * spacing * devicePixelRatio, arr[i], (i + 1) * spacing * devicePixelRatio, arr[i+1]);
		ctx.lineTo((i + 1) * spacing * devicePixelRatio, canvas.height + 1);
		ctx.lineTo(-1, canvas.height + 1);
		ctx.fillStyle = 'black';
		ctx.strokeStyle = 'white';
		ctx.lineWidth = devicePixelRatio;
		ctx.fill();
		ctx.stroke();
	})();

	load = e => {
		var u = location.hash.substring(1);
		media.src = 'https://r.6f.io?u=' + u;
		media.poster = 'https://r.6f.io?thumbnail=true&u=' + u;
	}

	preventDefault = e => {
		e.preventDefault();
		return false;
	}

	window.addEventListener('hashchange', load)
	document.addEventListener('dragover', preventDefault);
	document.addEventListener('dragenter', preventDefault);
	document.addEventListener('dragend', preventDefault);
	document.addEventListener('dragleave', preventDefault);
	document.addEventListener('drop', e => {
		e.preventDefault();
		var files = e.dataTransfer.files;
		if (!files.length) return false;
		URL.revokeObjectURL(media.src);
		media.src = URL.createObjectURL(files[0]);
		media.play();
		return false;
	});
	document.addEventListener('click', () => actx.resume(), { once: true });

	
	if (location.hash) load();
})();
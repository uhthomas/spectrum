(() => {
	const actx = new (window.AudioContext || window.webkitAudioContext)();
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

		const w = innerWidth;
		const h = innerHeight;

		const r = media.videoWidth / media.videoHeight;
		const nr = w / h;

		canvas.width = (nr > r ? h * r : w) * devicePixelRatio + .5 | 0;
		canvas.height = (nr < r ? w / r : h) * devicePixelRatio + .5 | 0;

		const x = (w - canvas.width) / 2 + .5 | 0;
		const y = (h - canvas.height) / 2 + .5 | 0;

		const scale = 1 / devicePixelRatio;
		
		canvas.style.transform = `matrix3d(
			${scale}, 0, 0, 0,
			0, ${scale}, 0, 0,
			0, 0, 1, 0,
			${x}, ${y}, 0, 1
		)`;

		// add 2 to compensate for start and end points then add .5 for rounding.
		var arr = new Float32Array(scale * canvas.width / spacing + 2.5 | 0);
		analyser.getFloatFrequencyData(arr);

		arr = arr.map(v => canvas.height - ((v - min) * (max - min) / 8) * devicePixelRatio);

		ctx.drawImage(media, 0, 0, canvas.width, canvas.height);

		ctx.beginPath();
		ctx.moveTo(0, arr[0]);
		var i = 1;
		for (; i < arr.length - 1; i++) {
			const x = i * spacing * devicePixelRatio;
			const x2 = (i + 1) * spacing * devicePixelRatio;
			const xc = (x + x2) / 2;

			const y = arr[i];
			const y2 = arr[i + 1];
			const yc = (y + y2) / 2;

			ctx.quadraticCurveTo(x, y, xc, yc);
		}
		ctx.quadraticCurveTo(i * spacing * devicePixelRatio, arr[i], (i + 1) * spacing * devicePixelRatio, arr[i+1]);
		ctx.lineTo((i + 1) * spacing * devicePixelRatio, canvas.height + 1);
		ctx.lineTo(-1, canvas.height + 1);

		ctx.strokeStyle = 'white';
		ctx.lineWidth = devicePixelRatio;
		ctx.stroke();

		ctx.globalCompositeOperation = 'destination-out';
		ctx.fill();
	})();

	load = e => {
		const raw = 'raw:';
		let u = location.hash.substring(1);
		if (u.startsWith(raw))
			return media.src = u.substr(raw.length);
		media.src = 'https://r.6f.io?u=' + u;
		media.poster = 'https://r.6f.io?thumbnail=true&u=' + u;
	}

	preventDefault = e => {
		e.preventDefault();
		return false;
	}

	window.addEventListener('hashchange', load)
	window.addEventListener('dragover', preventDefault);
	window.addEventListener('dragenter', preventDefault);
	window.addEventListener('dragend', preventDefault);
	window.addEventListener('dragleave', preventDefault);
	window.addEventListener('drop', e => {
		e.preventDefault();
		var files = e.dataTransfer.files;
		if (!files.length) return false;
		URL.revokeObjectURL(media.src);
		media.src = URL.createObjectURL(files[0]);
		media.play();
		return false;
	});
	window.addEventListener('click', () => actx.resume(), { once: true });

	
	if (location.hash) load();
})();
(() => {
	var actx = new AudioContext();
	var src = actx.createMediaElementSource(media);
	var analyser = actx.createAnalyser();
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
		canvas.width = innerWidth * devicePixelRatio;
		canvas.height = innerHeight * devicePixelRatio;

		var arr = new Float32Array((4 * (1 / devicePixelRatio) * canvas.width / spacing + 2) | 0);
		analyser.getFloatFrequencyData(arr);
		ctx.clearRect(0, 0, canvas.width, canvas.height);

		arr = arr.map(v => canvas.height - ((v - min) * (max - min) / 8) * devicePixelRatio);

		ctx.beginPath();
		ctx.moveTo(0, arr[0]);
		var i = 1;
		for (; i < arr.length - 2; i++) {
			var x = i * spacing * devicePixelRatio;
			var x2 = (i + 1) * spacing * devicePixelRatio;
			var y = arr[i];
			var y2 = arr[i + 1];
			var xc = (x + x2) / 2;
			var yc = (y + y2) / 2;
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
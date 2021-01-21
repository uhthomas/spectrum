let svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg')

let clipPath = document.createElementNS('http://www.w3.org/2000/svg', 'clipPath')
clipPath.setAttribute('id', 'clip')

svg.appendChild(clipPath)

let p = document.createElementNS('http://www.w3.org/2000/svg', 'path')
p.setAttribute('stroke', 'white')
p.setAttribute('stroke-width', '2')
p.setAttribute('fill', 'transparent')

clipPath.appendChild(p)

document.body.appendChild(svg)

const actx = new (window.AudioContext || window.webkitAudioContext)()
const src = actx.createMediaElementSource(media)
const analyser = actx.createAnalyser()
analyser.fftSize = {
    41000: 4 << 10,
    48000: 4 << 10,
    96000: 8 << 10,
    192000: 16 << 10,
    384000: 32 << 10
  }[actx.sampleRate] || 4 << 10
analyser.smoothingTimeConstant = .67
src.connect(analyser)
analyser.connect(actx.destination)

const min = -66
const max = 12
const spacing = 40;
(draw = () => {
  requestAnimationFrame(draw)

  const w = media.scrollWidth
  const h = media.scrollHeight

  const r = media.videoWidth / media.videoHeight
  const nr = w / h

  const contentWidth = (nr > r ? h * r : w) * devicePixelRatio
  const contentHeight = (nr < r ? w / r : h) * devicePixelRatio

  const x = (w - contentWidth) / 2
  const y = (h - contentHeight) / 2

  const scale = 1 / devicePixelRatio

  filter.style.width = contentWidth + 'px'
  filter.style.height = contentHeight + 'px'
  filter.style.transform = `matrix3d(
		${scale}, 0, 0, 0,
		0, ${scale}, 0, 0,
		0, 0, 1, 0,
		${x}, ${y}, 0, 1
	)`

  // add 2 to compensate for start and end points then add .5 for rounding.
  let arr = new Float32Array(scale * contentWidth / spacing + 2.5 | 0)
  analyser.getFloatFrequencyData(arr)

  if (arr.some(n => !isFinite(n))) {
    return
  }

  arr = arr.map(v => contentHeight - ((v - min) * (max - min) / 8) * devicePixelRatio)

  let d = `M 0 ${arr[0]} `
  for (let i = 1; i < arr.length - 1; i++) {
    const x = i * spacing * devicePixelRatio
    const x2 = (i + 1) * spacing * devicePixelRatio
    const xc = (x + x2) / 2

    const yc = (arr[i] + arr[i + 1]) / 2
    d += `Q ${x} ${arr[i]} ${xc} ${yc}, `
  }
  d += `L ${arr.length * spacing * devicePixelRatio} ${contentHeight}, L 0 ${contentHeight}, L 0 0`

  p.setAttribute('d', d)
})()

const load = e => {
  const raw = 'raw:'
  const u = location.hash.substring(1)
  if (u.startsWith(raw)) {
    return media.src = u.substr(raw.length)
  }
  media.src = 'https://r.6f.io?u=' + u
  media.poster = 'https://r.6f.io?thumbnail=true&u=' + u
}

const preventDefault = e => {
  e.preventDefault()
  return false
}

window.addEventListener('hashchange', load)
window.addEventListener('dragover', preventDefault)
window.addEventListener('dragenter', preventDefault)
window.addEventListener('dragend', preventDefault)
window.addEventListener('dragleave', preventDefault)
window.addEventListener('drop', e => {
  e.preventDefault()
  const files = e.dataTransfer.files
  if (!files.length) {
    return false
  }
  URL.revokeObjectURL(media.src)
  media.src = URL.createObjectURL(files[0])
  media.play()
  return false
})
window.addEventListener('click', () => actx.state !== 'running' && actx.resume(), {
  once: true
})


if (location.hash) {
  load()
}

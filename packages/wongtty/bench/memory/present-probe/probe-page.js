// Present-path probe: N panes of W x H redrawn when "dirty" (every INTERVAL ms each), presented in one of several ways.
//
// A mode is a present path with optional '+' flags, for bisecting what the GPU process keeps per pane:
//   alpha0  context alpha: false (WebGL, and the 2D pane canvases of copy)
//   pma0    WebGL premultipliedAlpha: false
//   pdb     WebGL preserveDrawingBuffer: true
//   aa      WebGL antialias: true
//   abs     the pane canvas is position:absolute at 0,0 in its position:relative host (as .xterm-screen canvas)
//   late    the context is made on the default 300x150 canvas and the backing size is set afterwards
//   ro      the backing size comes from ResizeObserver's device-pixel-content-box, as xterm.js's addon-webgl does
//   x1      the backing size is the CSS size (dpr 1 pixels) whatever the dpr
//   clear   gl.clear before every pane draw
// `xfer` presents the shared surface with transferToImageBitmap into 'bitmaprenderer' pane canvases.
// `xtermlike` is xterm.js addon-webgl's setup: perpane+abs+late+ro+clear (its context attributes are perpane's:
// antialias false, depth false, preserveDrawingBuffer false, alpha and premultipliedAlpha left at their defaults).
window.runProbe = async ({ mode: spec, n, w, h, interval, dpr, cols, rows, busyN }) => {
  let [mode, ...flags] = spec.split('+');
  if (mode === 'xtermlike') {
    mode = 'perpane';
    flags = ['abs', 'late', 'ro', 'clear', ...flags];
  }
  const has = (f) => flags.includes(f);
  const counts = { draws: 0, presents: 0, frames: 0 };
  const cells = cols * rows;
  const vs = `#version 300 es
  layout(location=0) in vec2 a_unit; layout(location=1) in vec4 a_rect; layout(location=2) in vec4 a_color;
  uniform vec2 u_size; out vec4 v_color;
  void main(){ vec2 p = a_rect.xy + a_unit * a_rect.zw; vec2 c = p / u_size * 2.0 - 1.0; gl_Position = vec4(c.x, -c.y, 0, 1); v_color = a_color; }`;
  const fs = `#version 300 es
  precision mediump float; in vec4 v_color; out vec4 o; void main(){ o = v_color; }`;
  function setupGl(gl) {
    const p = gl.createProgram();
    for (const [t, s] of [[gl.VERTEX_SHADER, vs], [gl.FRAGMENT_SHADER, fs]]) { const sh = gl.createShader(t); gl.shaderSource(sh, s); gl.compileShader(sh); gl.attachShader(p, sh); }
    gl.linkProgram(p); gl.useProgram(p);
    const vao = gl.createVertexArray(); gl.bindVertexArray(vao);
    const unit = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, unit);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([0,0,1,0,0,1,1,1]), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0); gl.vertexAttribPointer(0, 2, gl.FLOAT, false, 0, 0);
    const inst = gl.createBuffer(); gl.bindBuffer(gl.ARRAY_BUFFER, inst);
    gl.enableVertexAttribArray(1); gl.vertexAttribPointer(1, 4, gl.FLOAT, false, 32, 0); gl.vertexAttribDivisor(1, 1);
    gl.enableVertexAttribArray(2); gl.vertexAttribPointer(2, 4, gl.FLOAT, false, 32, 16); gl.vertexAttribDivisor(2, 1);
    return { size: gl.getUniformLocation(p, 'u_size'), inst };
  }
  const attrs = (preserve) => ({
    antialias: has('aa'),
    depth: false,
    preserveDrawingBuffer: !!preserve || has('pdb'),
    ...(has('alpha0') ? { alpha: false } : {}),
    ...(has('pma0') ? { premultipliedAlpha: false } : {}),
  });
  const W = Math.round(w * dpr), H = Math.round(h * dpr);
  // A pane's canvas as the flags say; `ready(c)` sets its backing size once its context exists.
  const observers = [];
  const paneCanvas = (p) => {
    const c = document.createElement('canvas');
    c.style.cssText = `width:${w}px;height:${h}px${has('abs') ? ';position:absolute;left:0;top:0' : ''}`;
    const size = () => { c.width = has('x1') ? w : W; c.height = has('x1') ? h : H; };
    if (!has('late')) size();
    p.host.appendChild(c);
    c.ready = () => {
      if (has('late')) size();
      if (has('ro')) {
        const ro = new ResizeObserver((es) => {
          const box = es[0].devicePixelContentBoxSize?.[0];
          if (!box) return;
          if (c.width !== box.inlineSize || c.height !== box.blockSize) { c.width = box.inlineSize; c.height = box.blockSize; p.dirty = true; }
        });
        ro.observe(c, { box: 'device-pixel-content-box' });
        observers.push(ro);
      }
    };
    return c;
  };
  // per-pane cell data
  const panes = [];
  let seed = 1;
  const rnd = () => ((seed = (seed * 1103515245 + 12345) & 0x7fffffff) / 0x7fffffff);
  for (let i = 0; i < n; i++) {
    const host = document.createElement('div');
    host.style.cssText = `display:inline-block;vertical-align:top;width:${w}px;height:${h}px;position:relative`;
    document.body.appendChild(host);
    panes.push({ host, data: new Float32Array(cells * 8), dirty: true, frame: 0 });
  }
  function fill(p) {
    const cw = W / cols, ch = H / rows;
    for (let c = 0; c < cells; c++) {
      const o = c * 8; p.data[o] = (c % cols) * cw; p.data[o + 1] = Math.floor(c / cols) * ch; p.data[o + 2] = cw * (0.4 + 0.6 * rnd()); p.data[o + 3] = ch;
      p.data[o + 4] = rnd(); p.data[o + 5] = rnd(); p.data[o + 6] = rnd(); p.data[o + 7] = 1;
    }
  }
  let present;
  if (mode === 'batch') {
    // one big offscreen surface with a tile per pane: dirty panes draw into their tiles, then one snapshot feeds every drawImage
    const per = Math.ceil(Math.sqrt(n));
    const off = new OffscreenCanvas(W * per, H * Math.ceil(n / per));
    const gl = off.getContext('webgl2', attrs(false));
    const g = setupGl(gl); gl.enable(gl.SCISSOR_TEST);
    panes.forEach((p, i) => { p.tx = (i % per) * W; p.ty = Math.floor(i / per) * H; });
    for (const p of panes) {
      const c = document.createElement('canvas'); c.width = W; c.height = H; c.style.cssText = `width:${w}px;height:${h}px`;
      p.host.appendChild(c); p.ctx = c.getContext('2d');
    }
    present = (p) => {
      const y = off.height - p.ty - H;
      gl.viewport(p.tx, y, W, H); gl.scissor(p.tx, y, W, H); gl.uniform2f(g.size, W, H);
      gl.bufferData(gl.ARRAY_BUFFER, p.data, gl.DYNAMIC_DRAW);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, cells); counts.draws++;
      p.pending = true;
    };
    present.flush = () => { for (const p of panes) if (p.pending) { p.pending = false; p.ctx.globalCompositeOperation = 'copy'; p.ctx.drawImage(off, p.tx, p.ty, W, H, 0, 0, W, H); counts.presents++; } };
    window.batchSurface = { w: off.width, h: off.height };
  } else if (mode === 'copy' || mode === 'copy2' || mode === 'copyna' || mode === 'copyds' || mode === 'copyel' || mode === 'copycpu') {
    const off = mode === 'copyel' ? document.createElement('canvas') : new OffscreenCanvas(1, 1);
    const gl = off.getContext('webgl2', attrs(false));
    const g = setupGl(gl);
    for (const p of panes) {
      const c = paneCanvas(p);
      p.ctx = c.getContext('2d', mode === 'copyna' || has('alpha0') ? { alpha: false } : mode === 'copyds' ? { desynchronized: true } : mode === 'copycpu' ? { willReadFrequently: true } : undefined);
      c.ready();
    }
    present = (p) => {
      const cw = p.ctx.canvas.width, ch = p.ctx.canvas.height;
      if (off.width !== cw || off.height !== ch) { off.width = cw; off.height = ch; }
      gl.viewport(0, 0, cw, ch); gl.uniform2f(g.size, W, H);
      if (has('clear')) gl.clear(gl.COLOR_BUFFER_BIT);
      gl.bufferData(gl.ARRAY_BUFFER, p.data, gl.DYNAMIC_DRAW);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, cells); counts.draws++;
      p.ctx.globalCompositeOperation = 'copy';
      p.ctx.drawImage(off, 0, 0); counts.presents++;
    };
  } else if (mode === 'xfer') {
    // one shared WebGL surface as copy, presented with a zero-copy transferToImageBitmap into each pane's
    // 'bitmaprenderer' canvas instead of a 2D drawImage
    const off = new OffscreenCanvas(W, H);
    const gl = off.getContext('webgl2', attrs(false));
    const g = setupGl(gl);
    for (const p of panes) {
      const c = paneCanvas(p);
      p.ctx = c.getContext('bitmaprenderer');
      c.ready();
    }
    present = (p) => {
      const cw = p.ctx.canvas.width, ch = p.ctx.canvas.height;
      if (off.width !== cw || off.height !== ch) { off.width = cw; off.height = ch; }
      gl.viewport(0, 0, cw, ch); gl.uniform2f(g.size, W, H);
      if (has('clear')) gl.clear(gl.COLOR_BUFFER_BIT);
      gl.bufferData(gl.ARRAY_BUFFER, p.data, gl.DYNAMIC_DRAW);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, cells); counts.draws++;
      p.ctx.transferFromImageBitmap(off.transferToImageBitmap()); counts.presents++;
    };
  } else if (mode === 'copygpu') {
    // WebGL renders as today; the present is a GPU copy into the pane's WebGPU canvas instead of a 2D drawImage
    const adapter = await navigator.gpu.requestAdapter();
    const device = await adapter.requestDevice();
    const format = navigator.gpu.getPreferredCanvasFormat();
    const off = new OffscreenCanvas(1, 1);
    const gl = off.getContext('webgl2', attrs(false));
    const g = setupGl(gl);
    panes.forEach((p) => {
      const c = document.createElement('canvas'); c.width = W; c.height = H; c.style.cssText = `width:${w}px;height:${h}px`;
      p.host.appendChild(c); p.gctx = c.getContext('webgpu');
      p.gctx.__cfg = { device, format, alphaMode: 'premultiplied', usage: GPUTextureUsage.RENDER_ATTACHMENT | GPUTextureUsage.COPY_DST };
      p.gctx.configure(p.gctx.__cfg); p.canvas = c;
    });
    present = (p) => {
      if (off.width !== W || off.height !== H) { off.width = W; off.height = H; }
      gl.viewport(0, 0, W, H); gl.uniform2f(g.size, W, H);
      gl.bufferData(gl.ARRAY_BUFFER, p.data, gl.DYNAMIC_DRAW);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, cells); counts.draws++;
      device.queue.copyExternalImageToTexture({ source: off }, { texture: p.gctx.getCurrentTexture(), premultipliedAlpha: true }, [W, H]);
      counts.presents++;
    };
    present.webgpu = true;
  } else if (mode === 'webgpu') {
    const adapter = await navigator.gpu.requestAdapter();
    const device = await adapter.requestDevice();
    const format = navigator.gpu.getPreferredCanvasFormat();
    const module = device.createShaderModule({ code: `
      struct U { size: vec2f };
      @group(0) @binding(0) var<uniform> u: U;
      struct O { @builtin(position) p: vec4f, @location(0) c: vec4f };
      @vertex fn vs(@builtin(vertex_index) vi: u32, @location(0) r: vec4f, @location(1) col: vec4f) -> O {
        let unit = vec2f(f32(vi & 1u), f32(vi >> 1u));
        let p = r.xy + unit * r.zw; let c = p / u.size * 2.0 - 1.0;
        var o: O; o.p = vec4f(c.x, -c.y, 0.0, 1.0); o.c = col; return o; }
      @fragment fn fs(i: O) -> @location(0) vec4f { return i.c; }` });
    const pipeline = device.createRenderPipeline({ layout: 'auto',
      vertex: { module, entryPoint: 'vs', buffers: [{ arrayStride: 32, stepMode: 'instance', attributes: [{ shaderLocation: 0, offset: 0, format: 'float32x4' }, { shaderLocation: 1, offset: 16, format: 'float32x4' }] }] },
      fragment: { module, entryPoint: 'fs', targets: [{ format }] }, primitive: { topology: 'triangle-strip' } });
    const ubuf = device.createBuffer({ size: 16, usage: GPUBufferUsage.UNIFORM | GPUBufferUsage.COPY_DST });
    device.queue.writeBuffer(ubuf, 0, new Float32Array([W, H, 0, 0]));
    const bind = device.createBindGroup({ layout: pipeline.getBindGroupLayout(0), entries: [{ binding: 0, resource: { buffer: ubuf } }] });
    const vbuf = device.createBuffer({ size: cells * 32 * n, usage: GPUBufferUsage.VERTEX | GPUBufferUsage.COPY_DST });
    panes.forEach((p, i) => {
      const c = document.createElement('canvas'); c.width = W; c.height = H; c.style.cssText = `width:${w}px;height:${h}px`;
      p.host.appendChild(c); p.gctx = c.getContext('webgpu'); p.gctx.__cfg = { device, format, alphaMode: 'premultiplied' }; p.gctx.configure(p.gctx.__cfg); p.off = i * cells * 32; p.canvas = c;
    });
    present = (p) => {
      device.queue.writeBuffer(vbuf, p.off, p.data);
      const enc = device.createCommandEncoder();
      const pass = enc.beginRenderPass({ colorAttachments: [{ view: p.gctx.getCurrentTexture().createView(), loadOp: 'clear', storeOp: 'store', clearValue: [0, 0, 0, 1] }] });
      pass.setPipeline(pipeline); pass.setBindGroup(0, bind); pass.setVertexBuffer(0, vbuf, p.off, cells * 32); pass.draw(4, cells); pass.end();
      device.queue.submit([enc.finish()]); counts.draws++; counts.presents++;
    };
    present.webgpu = true;
  } else if (mode === 'perpane') {
    for (const p of panes) {
      const c = paneCanvas(p);
      p.gl = c.getContext('webgl2', attrs(false)); p.g = setupGl(p.gl);
      c.ready();
    }
    present = (p) => {
      const gl = p.gl; if (gl.isContextLost()) return;
      gl.viewport(0, 0, gl.drawingBufferWidth, gl.drawingBufferHeight); gl.uniform2f(p.g.size, W, H);
      if (has('clear')) gl.clear(gl.COLOR_BUFFER_BIT);
      gl.bufferData(gl.ARRAY_BUFFER, p.data, gl.DYNAMIC_DRAW);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, cells); counts.draws++; counts.presents++;
    };
  } else if (mode === 'page' || mode === 'pagep') {
    // one canvas covering the bounding box of all panes, fixed over them
    const rects = panes.map((p) => p.host.getBoundingClientRect());
    const x0 = Math.min(...rects.map((r) => r.left)), y0 = Math.min(...rects.map((r) => r.top));
    const x1 = Math.max(...rects.map((r) => r.right)), y1 = Math.max(...rects.map((r) => r.bottom));
    const c = document.createElement('canvas');
    c.width = Math.round((x1 - x0) * dpr); c.height = Math.round((y1 - y0) * dpr);
    c.style.cssText = `position:absolute;left:${x0}px;top:${y0}px;width:${x1 - x0}px;height:${y1 - y0}px;pointer-events:none`;
    document.body.appendChild(c); window.pageEl = c;
    const gl = c.getContext('webgl2', attrs(mode === 'pagep'));
    const g = setupGl(gl);
    gl.enable(gl.SCISSOR_TEST);
    panes.forEach((p, i) => { const r = rects[i]; p.x = Math.round((r.left - x0) * dpr); p.y = c.height - Math.round((r.top - y0) * dpr) - H; });
    window.pageCanvas = { w: c.width, h: c.height };
    present = (p) => {
      gl.viewport(p.x, p.y, W, H); gl.scissor(p.x, p.y, W, H); gl.uniform2f(g.size, W, H);
      gl.bufferData(gl.ARRAY_BUFFER, p.data, gl.DYNAMIC_DRAW);
      gl.drawArraysInstanced(gl.TRIANGLE_STRIP, 0, 4, cells); counts.draws++;
    };
    present.page = true;
  }
  for (const p of panes) fill(p);
  let busy = false; let timer = null;
  const loop = () => {
    counts.frames++;
    const anyDirty = panes.some((p) => p.dirty);
    if (present.page && mode === 'page') {
      // drawing buffer is not preserved: any change redraws every pane
      if (anyDirty) { for (const p of panes) { present(p); p.dirty = false; } counts.presents++; }
    } else {
      for (const p of panes) {
        if (!p.dirty) continue;
        p.frame++;
        if (mode === 'copy2' && busy && performance.now() - (p.last ?? 0) < 100) continue; // at most every 100 ms
        p.last = performance.now();
        present(p); p.dirty = false;
      }
      if (present.page) counts.presents++;
      if (present.flush) present.flush();
    }
    requestAnimationFrame(loop);
  };
  requestAnimationFrame(loop);
  const p0ctx = () => !!panes[0].ctx;
  window.probe = {
    start() { busy = true; timer = setInterval(() => { for (const p of panes.slice(0, busyN ?? n)) { fill(p); p.dirty = true; } }, interval); },
    stop() { busy = false; clearInterval(timer); for (const p of panes) p.dirty = true; },
    counts() { const o = { ...counts }; counts.draws = counts.presents = counts.frames = 0; return o; },
    // Device pixels, in millions, of the pane canvases' backing stores and of the live WebGL drawing buffers.
    mpx() {
      let canvas = 0, gl = 0;
      for (const c of document.querySelectorAll('canvas')) canvas += c.width * c.height;
      for (const p of panes) if (p.gl && !p.gl.isContextLost()) gl += p.gl.drawingBufferWidth * p.gl.drawingBufferHeight;
      return { canvas: canvas / 1e6, gl: gl / 1e6, lost: panes.filter((p) => p.gl?.isContextLost()).length };
    },
    // compaction: as wongtty, resize pane canvases to 0 and back and redraw once
    compact() {
      if (present.webgpu && window.pageCompact) for (const p of panes) {
        if (window.pageCompact === 'unconf') { p.gctx.unconfigure(); p.gctx.configure(p.gctx.__cfg); present(p); }
        else { const c = p.canvas; const cw = c.width; c.width = 0; c.width = cw; present(p); }
      }
      if (p0ctx()) { for (const p of panes) { const c = p.ctx.canvas; const cw = c.width; c.width = 0; c.width = cw; present(p); } if (present.flush) present.flush(); }
      if (present.page && window.pageCompact) { const c = window.pageEl; const cw = c.width; c.width = 0; c.width = cw; for (const p of panes) present(p); }
    },
  };
  // initial frames
  await new Promise((r) => { let k = 20; const f = () => (k-- > 0 ? requestAnimationFrame(f) : r()); f(); });
  return { pageCanvas: window.pageCanvas ?? window.batchSurface ?? null };
};

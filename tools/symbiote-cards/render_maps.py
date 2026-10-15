"""Blender (5.2, Cycles CPU) renderer for the symbiote card maps (#1937, direction A shipped in #1957).

  blender --background --factory-startup --python render_maps.py -- <out_dir> [asset ...]

One fixed top orthographic camera (0,0,10), no rotation: +X right, +Y up, +Z toward the viewer.
Each asset is a heightfield mesh built from shapes.py; one multilayer EXR (float32, uncompressed)
per asset carries Combined alpha, Normal (camera = world space), AO and the AOVs
albedo / emit / thick / rough / fade / height. pack_maps.py turns them into the runtime WebP maps.
"""
import math
import os
import sys
import time

import bpy
import numpy as np

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import shapes  # noqa: E402

AOVS = [("albedo", "COLOR"), ("emit", "VALUE"), ("thick", "VALUE"), ("rough", "VALUE"), ("fade", "VALUE"), ("height", "VALUE")]


def reset_scene():
    bpy.ops.wm.read_factory_settings(use_empty=True)
    scene = bpy.context.scene
    scene.render.engine = "CYCLES"
    scene.cycles.device = "CPU"
    scene.cycles.use_denoising = False
    scene.cycles.use_adaptive_sampling = False
    scene.cycles.max_bounces = 0
    scene.render.film_transparent = True
    scene.render.resolution_percentage = 100
    scene.render.image_settings.media_type = "MULTI_LAYER_IMAGE"
    scene.render.image_settings.file_format = "OPEN_EXR_MULTILAYER"
    scene.render.image_settings.color_depth = "32"
    scene.render.image_settings.exr_codec = "NONE"
    world = bpy.data.worlds.new("w")
    scene.world = world
    world.color = (0, 0, 0)
    cam_data = bpy.data.cameras.new("cam")
    cam_data.type = "ORTHO"
    cam_data.clip_start = 0.1
    cam_data.clip_end = 100
    cam = bpy.data.objects.new("cam", cam_data)
    cam.location = (0, 0, 10)
    cam.rotation_euler = (0, 0, 0)
    scene.collection.objects.link(cam)
    scene.camera = cam
    vl = scene.view_layers[0]
    vl.use_pass_normal = True
    vl.use_pass_ambient_occlusion = True
    for name, kind in AOVS:
        aov = vl.aovs.add()
        aov.name = name
        aov.type = kind
    return scene, cam_data


def material():
    mat = bpy.data.materials.new("m")
    mat.use_nodes = True
    nt = mat.node_tree
    nt.nodes.clear()
    out = nt.nodes.new("ShaderNodeOutputMaterial")
    bsdf = nt.nodes.new("ShaderNodeBsdfPrincipled")
    nt.links.new(bsdf.outputs["BSDF"], out.inputs["Surface"])

    def attr(name):
        node = nt.nodes.new("ShaderNodeAttribute")
        node.attribute_type = "GEOMETRY"
        node.attribute_name = name
        return node

    albedo = attr("albedo")
    nt.links.new(albedo.outputs["Color"], bsdf.inputs["Base Color"])
    for name, kind in AOVS:
        node = nt.nodes.new("ShaderNodeOutputAOV")
        node.aov_name = name
        src = albedo.outputs["Color"] if kind == "COLOR" else attr(name).outputs["Fac"]
        nt.links.new(src, node.inputs["Color" if kind == "COLOR" else "Value"])
    return mat


def build_mesh(name, verts, faces_quads, faces_tris, values):
    mesh = bpy.data.meshes.new(name)
    nv = len(verts)
    mesh.vertices.add(nv)
    mesh.vertices.foreach_set("co", verts.astype(np.float32).ravel())
    loops = np.concatenate([faces_quads.ravel(), faces_tris.ravel()]).astype(np.int32)
    sizes = np.concatenate([np.full(len(faces_quads), 4), np.full(len(faces_tris), 3)]).astype(np.int32)
    starts = np.concatenate([[0], np.cumsum(sizes)[:-1]]).astype(np.int32)
    mesh.loops.add(len(loops))
    mesh.loops.foreach_set("vertex_index", loops)
    mesh.polygons.add(len(sizes))
    mesh.polygons.foreach_set("loop_start", starts)
    mesh.update(calc_edges=True)
    mesh.polygons.foreach_set("use_smooth", np.ones(len(sizes), bool))
    rgba = np.concatenate([values["albedo"], np.ones((nv, 1))], axis=1).astype(np.float32)
    col = mesh.attributes.new("albedo", "FLOAT_COLOR", "POINT")
    col.data.foreach_set("color", rgba.ravel())
    for key in ("emit", "thick", "rough", "fade", "height"):
        a = mesh.attributes.new(key, "FLOAT", "POINT")
        a.data.foreach_set("value", values[key].astype(np.float32))
    mesh.validate()
    return mesh


def tile_geometry(spec):
    res = spec["res"]
    margin = 0.25
    n = int(round(res * (1 + 2 * margin))) + 1
    lin = np.linspace(-0.5 - margin, 0.5 + margin, n)
    X, Y = np.meshgrid(lin, lin, indexing="xy")
    x = X.ravel()
    y = Y.ravel()
    v = spec["fn"](x, y)
    idx = np.arange(n * n).reshape(n, n)  # [row=y][col=x]
    a = idx[:-1, :-1].ravel()
    b = idx[:-1, 1:].ravel()
    c = idx[1:, 1:].ravel()
    d = idx[1:, :-1].ravel()
    quads = np.stack([a, b, c, d], axis=1)
    return x, y, v, quads, np.zeros((0, 3), np.int64)


def body_geometry(spec, angles=2048, rings=320, march=1400):
    half = spec["extent"] / 2
    fn = spec["fn"]
    th = np.linspace(0, 2 * np.pi, angles, endpoint=False)
    # last inside radius per angle (bodies are star-shaped around the origin, near enough)
    radii = np.linspace(0, half * 0.995, march)
    rmax = np.zeros(angles)
    for chunk in range(0, angles, 256):
        t = th[chunk:chunk + 256]
        R, T = np.meshgrid(radii, t, indexing="xy")
        inside = fn((R * np.cos(T)).ravel(), (R * np.sin(T)).ravel())["inside"].reshape(R.shape)
        last = np.where(inside, np.arange(march)[None, :], -1).max(axis=1)
        rmax[chunk:chunk + 256] = radii[np.maximum(last, 0)]
    s = 1 - (1 - np.arange(1, rings + 1) / rings) ** 1.6
    S, T = np.meshgrid(s, th, indexing="ij")  # [ring][angle]
    R = S * rmax[None, :]
    x = np.concatenate([[0.0], (R * np.cos(T)).ravel()])
    y = np.concatenate([[0.0], (R * np.sin(T)).ravel()])
    v = fn(x, y)
    inside = v["inside"]
    idx = 1 + np.arange(rings * angles).reshape(rings, angles)
    nxt = np.roll(idx, -1, axis=1)
    a = idx[:-1].ravel()
    b = idx[1:].ravel()
    c = nxt[1:].ravel()
    d = nxt[:-1].ravel()
    quads = np.stack([a, b, c, d], axis=1)
    quads = quads[inside[quads].all(axis=1)]
    ring0 = idx[0]
    tris = np.stack([np.zeros(angles, np.int64), ring0, np.roll(ring0, -1)], axis=1)
    tris = tris[inside[tris].all(axis=1)]
    return x, y, v, quads, tris


def render(name, out_dir):
    spec = shapes.ASSETS[name]
    t0 = time.time()
    scene, cam = reset_scene()
    geo = tile_geometry(spec) if spec["tile"] else body_geometry(spec)
    x, y, v, quads, tris = geo
    h = v["h"].astype(np.float64)
    if not spec["tile"]:
        h = np.where(v["inside"], h, 0.0)
    values = dict(v)
    values["height"] = h
    verts = np.stack([x, y, h], axis=1)
    mesh = build_mesh(name, verts, quads, tris, values)
    mesh.materials.append(material())
    obj = bpy.data.objects.new(name, mesh)
    scene.collection.objects.link(obj)
    cam.ortho_scale = spec["extent"]
    scene.render.resolution_x = spec["res"]
    scene.render.resolution_y = spec["res"]
    scene.cycles.samples = spec["samples"]
    scene.world.light_settings.distance = spec["ao"]
    scene.render.filepath = os.path.join(out_dir, name + ".exr")
    bpy.ops.render.render(write_still=True)
    print(f"[render] {name}: {len(verts)} verts, {len(quads) + len(tris)} faces, h {h.min():.4f}..{h.max():.4f}, {time.time() - t0:.1f}s", flush=True)


def main():
    argv = sys.argv[sys.argv.index("--") + 1:] if "--" in sys.argv else []
    out_dir = os.path.abspath(argv[0])
    names = argv[1:] or list(shapes.ASSETS)
    os.makedirs(out_dir, exist_ok=True)
    for name in names:
        render(name, out_dir)


main()

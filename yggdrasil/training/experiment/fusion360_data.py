#!/usr/bin/env python3
"""Infinite synthetic Fusion 360 Python API code generator — v2.

v1 had 10 fixed templates with only dimension variation. The model memorized
them perfectly (99% API, 85.5% syntax) but couldn't improve further.

v2 fixes this with:
  1. COMPOSABLE ARCHITECTURE: base shape + random feature stack
  2. CODE VARIATION: multiple implementation patterns per operation
  3. VARIABLE NAMING: randomized names (sketch vs sk vs baseSketch...)
  4. BOILERPLATE VARIATION: different error handling, imports, casting
  5. 15+ base shapes + 10+ feature modifiers = hundreds of combinations

Usage:
    from fusion360_data import Fusion360StreamDataset, generate_fusion_eval_set

    train_ds = Fusion360StreamDataset(difficulty=0, seed=42)
    eval_ds = generate_fusion_eval_set(n=200, seed=9999)

    python3 fusion360_data.py --preview 10
    python3 fusion360_data.py --preview-code 3
    python3 fusion360_data.py --stats
"""

import argparse
import json
import math
import random
from dataclasses import dataclass, field
from typing import Iterator, Optional

import torch
from torch.utils.data import IterableDataset


# ── Data Classes ────────────────────────────────────────────────

@dataclass
class CADProblem:
    description: str
    code: str
    operation: str
    difficulty: int
    features: list[str] = field(default_factory=list)


# ── Helpers ─────────────────────────────────────────────────────

def _dim(rng: random.Random, lo: float, hi: float, d: int = 1) -> float:
    return round(rng.uniform(lo, hi), d)

def _idim(rng: random.Random, lo: int, hi: int) -> int:
    return rng.randint(lo, hi)


# ── Naming Randomization ───────────────────────────────────────

class Names:
    """Randomized variable names so the model can't rely on exact names."""

    def __init__(self, rng: random.Random):
        self.rng = rng
        self._sketch_idx = 0
        self._ext_idx = 0

    def app(self) -> str:
        return self.rng.choice(["app", "application", "fusionApp"])

    def design(self) -> str:
        return self.rng.choice(["design", "des", "activeDesign"])

    def root(self) -> str:
        return self.rng.choice(["rootComp", "root", "rootComponent", "comp"])

    def sketch(self, label: str = "") -> str:
        self._sketch_idx += 1
        opts = [f"sketch{self._sketch_idx}", f"sk{self._sketch_idx}",
                f"{label}Sketch" if label else f"sketch{self._sketch_idx}",
                f"sk_{label}" if label else f"sketch_{self._sketch_idx}"]
        return self.rng.choice(opts)

    def profile(self, idx: int = 0) -> str:
        return self.rng.choice([f"prof", f"profile", f"prof{idx}", f"sketchProfile"])

    def extrude(self, label: str = "") -> str:
        self._ext_idx += 1
        opts = [f"ext{self._ext_idx}", f"extrude{self._ext_idx}",
                f"{label}Ext" if label else f"ext{self._ext_idx}",
                f"extFeature{self._ext_idx}"]
        return self.rng.choice(opts)

    def point(self, x, y, z=0) -> str:
        return f"adsk.core.Point3D.create({x}, {y}, {z})"

    def value(self, v) -> str:
        style = self.rng.randint(0, 1)
        if style == 0:
            return f"adsk.core.ValueInput.createByReal({v})"
        else:
            return f"adsk.core.ValueInput.createByString(\"{v} cm\")"

    def distance(self, v) -> str:
        return self.value(v)


# ── Boilerplate Variation ───────────────────────────────────────

def _boilerplate_start(rng: random.Random, n: Names) -> str:
    style = rng.randint(0, 3)
    app = n.app()
    des = n.design()
    root = n.root()

    if style == 0:
        return f"""import adsk.core, adsk.fusion, traceback

def run(context):
    {app} = adsk.core.Application.get()
    {des} = {app}.activeProduct
    {root} = {des}.rootComponent

    try:
"""
    elif style == 1:
        return f"""import adsk.core, adsk.fusion, adsk.cam, traceback

def run(context):
    ui = None
    try:
        {app} = adsk.core.Application.get()
        ui = {app}.userInterface
        {des} = adsk.fusion.Design.cast({app}.activeProduct)
        {root} = {des}.rootComponent

"""
    elif style == 2:
        return f"""import adsk.core
import adsk.fusion
import traceback

def run(context):
    {app} = adsk.core.Application.get()
    {des} = {app}.activeProduct
    {root} = {des}.rootComponent

    try:
"""
    else:
        return f"""import adsk.core, adsk.fusion, traceback

def run(context):
    try:
        {app} = adsk.core.Application.get()
        {des} = adsk.fusion.Design.cast({app}.activeProduct)
        {root} = {des}.rootComponent

"""


def _boilerplate_end(rng: random.Random) -> str:
    style = rng.randint(0, 2)
    if style == 0:
        return """
    except:
        app.userInterface.messageBox(traceback.format_exc())
"""
    elif style == 1:
        return """
    except Exception as e:
        if ui:
            ui.messageBox(f'Error: {e}\\n{traceback.format_exc()}')
"""
    else:
        return """
    except:
        traceback.print_exc()
"""


# ── Plane Selection ─────────────────────────────────────────────

def _plane(rng: random.Random, root: str) -> tuple[str, str]:
    """Return (plane_expr, plane_name)."""
    planes = [
        (f"{root}.xYConstructionPlane", "XY"),
        (f"{root}.xZConstructionPlane", "XZ"),
        (f"{root}.yZConstructionPlane", "YZ"),
    ]
    return rng.choice(planes)


# ── Extrude Patterns ────────────────────────────────────────────

def _extrude_code(rng: random.Random, n: Names, root: str,
                  prof_var: str, dist: float, op: str = "NewBody",
                  ext_var: str = "") -> str:
    """Generate extrude code with variation."""
    if not ext_var:
        ext_var = n.extrude()

    op_map = {
        "NewBody": "adsk.fusion.FeatureOperations.NewBodyFeatureOperation",
        "Join": "adsk.fusion.FeatureOperations.JoinFeatureOperation",
        "Cut": "adsk.fusion.FeatureOperations.CutFeatureOperation",
    }
    op_str = op_map.get(op, op_map["NewBody"])

    style = rng.randint(0, 1)
    if style == 0:
        # Simple API
        return f"""        extrudes = {root}.features.extrudeFeatures
        {ext_var} = extrudes.addSimple({prof_var}, {n.distance(dist)}, {op_str})
"""
    else:
        # Full API
        return f"""        extrudes = {root}.features.extrudeFeatures
        extInput = extrudes.createInput({prof_var}, {op_str})
        extInput.setDistanceExtent(False, {n.distance(dist)})
        {ext_var} = extrudes.add(extInput)
"""


# ── Description Phrasing ────────────────────────────────────────

_VERBS = ["Create", "Make", "Design", "Model", "Generate", "Build",
          "Draw", "Construct", "Produce", "Sketch up"]

def _verb(rng: random.Random) -> str:
    return rng.choice(_VERBS)


# ── BASE SHAPES ─────────────────────────────────────────────────

def gen_box(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    w, h, depth = _dim(rng, 1, 20), _dim(rng, 1, 20), _dim(rng, 1, 15)
    root = n.root()
    sk = n.sketch("base")
    prof = n.profile()
    ext = n.extrude("box")
    plane_expr, plane_name = _plane(rng, root)

    descs = [
        f"{_verb(rng)} a rectangular box that is {w}cm wide, {h}cm tall, and {depth}cm deep",
        f"{_verb(rng)} a {w} x {h} x {depth} cm box",
        f"{_verb(rng)} a cuboid with width={w}cm, height={h}cm, depth={depth}cm",
        f"I need a box: {w}cm by {h}cm by {depth}cm",
        f"{_verb(rng)} a rectangular block measuring {w} by {h} by {depth} centimeters",
        f"Box dimensions: {w}cm wide, {h}cm high, {depth}cm deep. {_verb(rng)} it.",
    ]

    # Vary rectangle drawing method
    rect_style = rng.randint(0, 1)
    if rect_style == 0:
        draw = f"""        lines = {sk}.sketchCurves.sketchLines
        lines.addTwoPointRectangle({n.point(0, 0)}, {n.point(w, h)})
"""
    else:
        draw = f"""        lines = {sk}.sketchCurves.sketchLines
        lines.addByTwoPoints({n.point(0, 0)}, {n.point(w, 0)})
        lines.addByTwoPoints({n.point(w, 0)}, {n.point(w, h)})
        lines.addByTwoPoints({n.point(w, h)}, {n.point(0, h)})
        lines.addByTwoPoints({n.point(0, h)}, {n.point(0, 0)})
"""

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({plane_expr})

{draw}
        {prof} = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, prof, depth, "NewBody", ext)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "box", d)


def gen_cylinder(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    radius = _dim(rng, 0.5, 10)
    height = _dim(rng, 1, 20)
    diameter = round(radius * 2, 1)
    root = n.root()
    sk = n.sketch("base")
    prof = n.profile()
    ext = n.extrude("cyl")
    plane_expr, _ = _plane(rng, root)

    descs = [
        f"{_verb(rng)} a cylinder with radius {radius}cm and height {height}cm",
        f"{_verb(rng)} a cylinder, diameter {diameter}cm, {height}cm tall",
        f"I need a cylindrical shape: r={radius}cm, h={height}cm",
        f"{_verb(rng)} a {diameter}cm diameter, {height}cm tall cylinder",
        f"Cylinder: radius {radius}cm, height {height}cm",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({plane_expr})
        {sk}.sketchCurves.sketchCircles.addByCenterRadius({n.point(0, 0)}, {radius})

        {prof} = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, prof, height, "NewBody", ext)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "cylinder", d)


def gen_sphere(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    radius = _dim(rng, 0.5, 10)
    root = n.root()
    sk = n.sketch("base")

    descs = [
        f"{_verb(rng)} a sphere with radius {radius}cm",
        f"{_verb(rng)} a {round(radius*2, 1)}cm diameter sphere",
        f"Sphere, radius={radius}cm",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({root}.xZConstructionPlane)

        circles = {sk}.sketchCurves.sketchCircles
        circles.addByCenterRadius({n.point(0, 0)}, {radius})

        axisLine = {sk}.sketchCurves.sketchLines.addByTwoPoints(
            {n.point(0, -radius)}, {n.point(0, radius)}
        )
        axisLine.isConstruction = True

        prof = {sk}.profiles.item(0)
        revolves = {root}.features.revolveFeatures
        revInput = revolves.createInput(prof, axisLine, adsk.fusion.FeatureOperations.NewBodyFeatureOperation)
        revInput.setAngleExtent(False, adsk.core.ValueInput.createByString("360 deg"))
        revolves.add(revInput)
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "sphere", d)


def gen_cone(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    base_r = _dim(rng, 1, 8)
    height = _dim(rng, 2, 15)
    top_r = _dim(rng, 0, base_r * 0.4)
    root = n.root()
    sk = n.sketch("profile")
    shape = "cone" if top_r < 0.2 else "truncated cone"
    top_r = 0 if top_r < 0.2 else top_r

    descs = [
        f"{_verb(rng)} a {shape} with base radius {base_r}cm and height {height}cm" +
        (f" and top radius {top_r}cm" if top_r > 0 else ""),
        f"{shape.capitalize()}: base r={base_r}cm, h={height}cm" +
        (f", top r={top_r}cm" if top_r > 0 else ""),
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({root}.xZConstructionPlane)

        lines = {sk}.sketchCurves.sketchLines
        lines.addByTwoPoints({n.point(0, 0)}, {n.point(base_r, 0)})
        lines.addByTwoPoints({n.point(base_r, 0)}, {n.point(top_r, height)})
        lines.addByTwoPoints({n.point(top_r, height)}, {n.point(0, height)})
        lines.addByTwoPoints({n.point(0, height)}, {n.point(0, 0)})

        prof = {sk}.profiles.item(0)
        revolves = {root}.features.revolveFeatures
        revInput = revolves.createInput(prof, {root}.yConstructionAxis, adsk.fusion.FeatureOperations.NewBodyFeatureOperation)
        revInput.setAngleExtent(False, adsk.core.ValueInput.createByString("360 deg"))
        revolves.add(revInput)
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "cone", d)


def gen_polygon_prism(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    sides = _idim(rng, 5, 8)
    radius = _dim(rng, 1, 8)
    height = _dim(rng, 1, 12)
    root = n.root()
    sk = n.sketch("base")
    prof = n.profile()
    ext = n.extrude("prism")
    plane_expr, _ = _plane(rng, root)
    shape_names = {5: "pentagonal", 6: "hexagonal", 7: "heptagonal", 8: "octagonal"}
    shape = shape_names.get(sides, f"{sides}-sided")

    descs = [
        f"{_verb(rng)} a {shape} prism with circumradius {radius}cm and height {height}cm",
        f"{_verb(rng)} a {sides}-sided prism, radius {radius}cm, {height}cm tall",
    ]

    # Generate polygon points
    points_code = ""
    for i in range(sides):
        angle = 2 * math.pi * i / sides
        x = round(radius * math.cos(angle), 3)
        y = round(radius * math.sin(angle), 3)
        angle_next = 2 * math.pi * ((i + 1) % sides) / sides
        x2 = round(radius * math.cos(angle_next), 3)
        y2 = round(radius * math.sin(angle_next), 3)
        points_code += f"        lines.addByTwoPoints({n.point(x, y)}, {n.point(x2, y2)})\n"

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({plane_expr})
        lines = {sk}.sketchCurves.sketchLines
{points_code}
        {prof} = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, prof, height, "NewBody", ext)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "polygon_prism", d)


def gen_torus(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    major_r = _dim(rng, 3, 10)
    minor_r = _dim(rng, 0.5, major_r * 0.4)
    root = n.root()
    sk = n.sketch("profile")

    descs = [
        f"{_verb(rng)} a torus with major radius {major_r}cm and minor radius {minor_r}cm",
        f"{_verb(rng)} a donut shape: ring radius {major_r}cm, tube radius {minor_r}cm",
        f"Torus: R={major_r}cm, r={minor_r}cm",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({root}.xZConstructionPlane)

        {sk}.sketchCurves.sketchCircles.addByCenterRadius(
            {n.point(major_r, 0)}, {minor_r}
        )

        prof = {sk}.profiles.item(0)
        revolves = {root}.features.revolveFeatures
        revInput = revolves.createInput(prof, {root}.yConstructionAxis, adsk.fusion.FeatureOperations.NewBodyFeatureOperation)
        revInput.setAngleExtent(False, adsk.core.ValueInput.createByString("360 deg"))
        revolves.add(revInput)
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "torus", d)


def gen_wedge(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    w = _dim(rng, 2, 12)
    h = _dim(rng, 2, 10)
    depth = _dim(rng, 2, 10)
    root = n.root()
    sk = n.sketch("base")
    prof = n.profile()
    ext = n.extrude("wedge")
    plane_expr, _ = _plane(rng, root)

    descs = [
        f"{_verb(rng)} a wedge: base {w}cm, height {h}cm, depth {depth}cm",
        f"{_verb(rng)} a triangular prism (wedge) {w}x{h}x{depth}cm",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({plane_expr})
        lines = {sk}.sketchCurves.sketchLines
        lines.addByTwoPoints({n.point(0, 0)}, {n.point(w, 0)})
        lines.addByTwoPoints({n.point(w, 0)}, {n.point(0, h)})
        lines.addByTwoPoints({n.point(0, h)}, {n.point(0, 0)})

        {prof} = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, prof, depth, "NewBody", ext)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "wedge", d)


# ── FEATURE MODIFIERS (composable) ──────────────────────────────

def _add_fillet(rng: random.Random, n: Names, root: str,
                body_var: str, radius: float) -> tuple[str, str]:
    """Returns (code_snippet, description_snippet)."""
    code = f"""
        # Fillet edges
        filletEdges = adsk.core.ObjectCollection.create()
        {body_var}Body = {body_var}.bodies.item(0)
        for edge in {body_var}Body.edges:
            filletEdges.add(edge)
        filletInput = {root}.features.filletFeatures.createInput()
        filletInput.addConstantRadiusEdgeSet(filletEdges, {n.value(radius)}, True)
        {root}.features.filletFeatures.add(filletInput)
"""
    desc = f" with {radius}cm rounded edges"
    return code, desc


def _add_chamfer(rng: random.Random, n: Names, root: str,
                 body_var: str, size: float) -> tuple[str, str]:
    code = f"""
        # Chamfer edges
        chamEdges = adsk.core.ObjectCollection.create()
        {body_var}Body = {body_var}.bodies.item(0)
        for edge in {body_var}Body.edges:
            chamEdges.add(edge)
        chamInput = {root}.features.chamferFeatures.createInput2()
        chamInput.chamferType = adsk.fusion.ChamferType.EqualDistanceChamferType
        chamInput.addToEdgeSets(chamEdges, {n.value(size)})
        {root}.features.chamferFeatures.add(chamInput)
"""
    desc = f" with {size}cm chamfers"
    return code, desc


def _add_shell(rng: random.Random, n: Names, root: str,
               body_var: str, thickness: float) -> tuple[str, str]:
    code = f"""
        # Shell (hollow out)
        {body_var}Body = {body_var}.bodies.item(0)
        topFace = {body_var}Body.faces.item(0)
        shellFaces = adsk.core.ObjectCollection.create()
        shellFaces.add(topFace)
        shellInput = {root}.features.shellFeatures.createInput(shellFaces, False, {n.value(thickness)})
        {root}.features.shellFeatures.add(shellInput)
"""
    desc = f", hollowed out with {thickness}cm wall thickness"
    return code, desc


def _add_center_hole(rng: random.Random, n: Names, root: str,
                     body_var: str, hole_r: float) -> tuple[str, str]:
    sk = n.sketch("hole")
    code = f"""
        # Center hole
        {sk} = {root}.sketches.add({body_var}.endFaces.item(0))
        {sk}.sketchCurves.sketchCircles.addByCenterRadius({n.point(0, 0)}, {hole_r})
        holeProf = {sk}.profiles.item(0)
        holeInput = {root}.features.extrudeFeatures.createInput(holeProf, adsk.fusion.FeatureOperations.CutFeatureOperation)
        holeInput.setAllExtent(adsk.fusion.ExtentDirections.NegativeExtentDirection)
        {root}.features.extrudeFeatures.add(holeInput)
"""
    desc = f" with a {hole_r}cm radius center hole"
    return code, desc


def _add_rectangular_pattern(rng: random.Random, n: Names, root: str,
                             feature_var: str, nx: int, ny: int,
                             dx: float, dy: float) -> tuple[str, str]:
    code = f"""
        # Rectangular pattern
        patEntities = adsk.core.ObjectCollection.create()
        patEntities.add({feature_var})
        rectPatterns = {root}.features.rectangularPatternFeatures
        patInput = rectPatterns.createInput(patEntities, {root}.xConstructionAxis)
        patInput.quantityOne = adsk.core.ValueInput.createByString("{nx}")
        patInput.distanceOne = {n.value(dx)}
        patInput.directionTwoEntity = {root}.yConstructionAxis
        patInput.quantityTwo = adsk.core.ValueInput.createByString("{ny}")
        patInput.distanceTwo = {n.value(dy)}
        rectPatterns.add(patInput)
"""
    desc = f" in a {nx}x{ny} grid pattern (spacing {dx}x{dy}cm)"
    return code, desc


def _add_mirror(rng: random.Random, n: Names, root: str,
                body_var: str) -> tuple[str, str]:
    planes = [
        (f"{root}.xYConstructionPlane", "XY"),
        (f"{root}.xZConstructionPlane", "XZ"),
        (f"{root}.yZConstructionPlane", "YZ"),
    ]
    plane_expr, plane_name = rng.choice(planes)
    code = f"""
        # Mirror across {plane_name} plane
        mirrorEntities = adsk.core.ObjectCollection.create()
        mirrorEntities.add({body_var}.bodies.item(0))
        mirrorInput = {root}.features.mirrorFeatures.createInput(mirrorEntities, {plane_expr})
        {root}.features.mirrorFeatures.add(mirrorInput)
"""
    desc = f", mirrored across the {plane_name} plane"
    return code, desc


# ── COMPOUND GENERATORS (base + random features) ───────────────

def gen_compound(rng: random.Random, d: int) -> CADProblem:
    """Generate a base shape with 1-3 random features applied."""
    # Pick base
    base_gen = rng.choice([gen_box, gen_cylinder])
    base = base_gen(rng, d)

    # How many features to add
    n_features = rng.randint(1, min(d, 3))
    n = Names(rng)
    root = "rootComp"  # standardize for feature code

    # Detect body variable from base code
    import re
    ext_match = re.search(r'(\w+) = extrudes\.add', base.code)
    body_var = ext_match.group(1) if ext_match else "ext1"

    feature_code = ""
    feature_descs = []
    used_features = set()

    available = ["fillet", "chamfer", "shell", "hole", "mirror"]
    for _ in range(n_features):
        remaining = [f for f in available if f not in used_features]
        if not remaining:
            break
        feat = rng.choice(remaining)
        used_features.add(feat)

        if feat == "fillet":
            fc, fd = _add_fillet(rng, n, root, body_var, _dim(rng, 0.1, 1.5))
        elif feat == "chamfer":
            fc, fd = _add_chamfer(rng, n, root, body_var, _dim(rng, 0.1, 1.0))
        elif feat == "shell":
            fc, fd = _add_shell(rng, n, root, body_var, _dim(rng, 0.1, 0.5))
        elif feat == "hole":
            fc, fd = _add_center_hole(rng, n, root, body_var, _dim(rng, 0.3, 2.0))
        elif feat == "mirror":
            fc, fd = _add_mirror(rng, n, root, body_var)
        else:
            continue

        feature_code += fc
        feature_descs.append(fd)

    # Insert feature code before the except block
    parts = base.code.rsplit("\n    except", 1)
    if len(parts) == 2:
        combined_code = parts[0] + feature_code + "\n    except" + parts[1]
    else:
        combined_code = base.code + feature_code

    combined_desc = base.description + "".join(feature_descs)
    return CADProblem(combined_desc, combined_code,
                      f"{base.operation}_compound", d,
                      features=list(used_features))


# ── L-BRACKET (fixed from v1 — multiple code patterns) ─────────

def gen_l_bracket(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    h1 = _dim(rng, 3, 12)
    h2 = _dim(rng, 2, 8)
    thick = _dim(rng, 0.3, 1.5)
    depth = _dim(rng, 2, 8)
    root = n.root()
    sk = n.sketch("profile")
    prof = n.profile()
    ext = n.extrude("bracket")
    plane_expr, _ = _plane(rng, root)

    descs = [
        f"{_verb(rng)} an L-bracket: vertical {h1}cm, horizontal {h2}cm, {thick}cm thick, {depth}cm deep",
        f"{_verb(rng)} an L-shaped bracket with {h1}cm and {h2}cm legs, {thick}cm wall, extruded {depth}cm",
        f"L-bracket: {h1}x{h2}cm legs, thickness {thick}cm, depth {depth}cm",
    ]

    # Two different ways to draw the L-shape
    style = rng.randint(0, 1)
    if style == 0:
        # 6-line closed profile
        draw = f"""        lines = {sk}.sketchCurves.sketchLines
        p0 = {n.point(0, 0)}
        p1 = {n.point(h2, 0)}
        p2 = {n.point(h2, thick)}
        p3 = {n.point(thick, thick)}
        p4 = {n.point(thick, h1)}
        p5 = {n.point(0, h1)}
        lines.addByTwoPoints(p0, p1)
        lines.addByTwoPoints(p1, p2)
        lines.addByTwoPoints(p2, p3)
        lines.addByTwoPoints(p3, p4)
        lines.addByTwoPoints(p4, p5)
        lines.addByTwoPoints(p5, p0)
"""
    else:
        # Two rectangles approach (boolean join)
        draw = f"""        # Horizontal leg
        lines = {sk}.sketchCurves.sketchLines
        lines.addTwoPointRectangle({n.point(0, 0)}, {n.point(h2, thick)})

        # Vertical leg
        lines.addTwoPointRectangle({n.point(0, 0)}, {n.point(thick, h1)})
"""

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({plane_expr})

{draw}
        {prof} = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, prof, depth, "NewBody", ext)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "l_bracket", d)


# ── PLATE WITH HOLES ───────────────────────────────────────────

def gen_plate_with_holes(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    w, h = _dim(rng, 5, 20), _dim(rng, 5, 20)
    thick = _dim(rng, 0.3, 2)
    hole_r = _dim(rng, 0.2, min(w, h) * 0.08)
    n_holes = _idim(rng, 2, 6)
    root = n.root()
    sk = n.sketch("plate")
    prof = n.profile()
    ext = n.extrude("plate")
    plane_expr, _ = _plane(rng, root)

    margin = max(hole_r * 3, 1.0)
    holes = [(_dim(rng, margin, w - margin), _dim(rng, margin, h - margin))
             for _ in range(n_holes)]

    descs = [
        f"{_verb(rng)} a {w}x{h}cm plate, {thick}cm thick, with {n_holes} holes of radius {hole_r}cm",
        f"{_verb(rng)} a mounting plate: {w}x{h}x{thick}cm with {n_holes} mounting holes (r={hole_r}cm)",
    ]

    hole_code = ""
    for i, (hx, hy) in enumerate(holes):
        hsk = n.sketch(f"hole{i}")
        hole_code += f"""
        {hsk} = {root}.sketches.add({ext}.endFaces.item(0))
        {hsk}.sketchCurves.sketchCircles.addByCenterRadius({n.point(hx, hy)}, {hole_r})
        hProf{i} = {hsk}.profiles.item(0)
        hInput{i} = {root}.features.extrudeFeatures.createInput(hProf{i}, adsk.fusion.FeatureOperations.CutFeatureOperation)
        hInput{i}.setAllExtent(adsk.fusion.ExtentDirections.NegativeExtentDirection)
        {root}.features.extrudeFeatures.add(hInput{i})
"""

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({plane_expr})
        {sk}.sketchCurves.sketchLines.addTwoPointRectangle({n.point(0, 0)}, {n.point(w, h)})

        {prof} = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, prof, thick, "NewBody", ext)}
{hole_code}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "plate_holes", d)


# ── CIRCULAR PATTERN ────────────────────────────────────────────

def gen_circular_pattern(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    base_r = _dim(rng, 5, 15)
    base_h = _dim(rng, 0.5, 3)
    pin_r = _dim(rng, 0.3, 1.5)
    pin_h = _dim(rng, 1, 5)
    n_pins = _idim(rng, 3, 8)
    pin_dist = _dim(rng, 2, base_r * 0.8)
    root = n.root()
    sk1 = n.sketch("base")
    sk2 = n.sketch("pin")
    ext1 = n.extrude("base")
    ext2 = n.extrude("pin")

    descs = [
        f"{_verb(rng)} a disc (r={base_r}cm, h={base_h}cm) with {n_pins} pins "
        f"(r={pin_r}cm, h={pin_h}cm) at radius {pin_dist}cm",
        f"{_verb(rng)} a circular base with {n_pins} evenly-spaced pegs",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches

        # Base disc
        {sk1} = sketches.add({root}.xYConstructionPlane)
        {sk1}.sketchCurves.sketchCircles.addByCenterRadius({n.point(0, 0)}, {base_r})
        baseProf = {sk1}.profiles.item(0)
{_extrude_code(rng, n, root, "baseProf", base_h, "NewBody", ext1)}

        # Single pin
        {sk2} = sketches.add({ext1}.endFaces.item(0))
        {sk2}.sketchCurves.sketchCircles.addByCenterRadius({n.point(pin_dist, 0)}, {pin_r})
        pinProf = {sk2}.profiles.item(0)
{_extrude_code(rng, n, root, "pinProf", pin_h, "Join", ext2)}

        # Circular pattern
        patEntities = adsk.core.ObjectCollection.create()
        patEntities.add({ext2})
        circPatterns = {root}.features.circularPatternFeatures
        patInput = circPatterns.createInput(patEntities, {root}.zConstructionAxis)
        patInput.quantity = adsk.core.ValueInput.createByString("{n_pins}")
        patInput.totalAngle = adsk.core.ValueInput.createByString("360 deg")
        patInput.isSymmetric = False
        circPatterns.add(patInput)
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "circular_pattern", d)


# ── SLOT ────────────────────────────────────────────────────────

def gen_slot(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    length = _dim(rng, 2, 10)
    width = _dim(rng, 0.5, 3)
    plate_w = _dim(rng, max(length + 3, 5), 20)
    plate_h = _dim(rng, max(width + 3, 5), 15)
    plate_t = _dim(rng, 0.5, 3)
    root = n.root()
    sk = n.sketch("plate")
    sk2 = n.sketch("slot")
    ext = n.extrude("plate")
    half_w = round(width / 2, 2)

    descs = [
        f"{_verb(rng)} a {plate_w}x{plate_h}x{plate_t}cm plate with a {length}x{width}cm slot in the center",
        f"{_verb(rng)} a plate ({plate_w}x{plate_h}cm, {plate_t}cm thick) with a centered slot ({length}cm long, {width}cm wide)",
    ]

    cx = round(plate_w / 2, 1)
    cy = round(plate_h / 2, 1)

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({root}.xYConstructionPlane)
        {sk}.sketchCurves.sketchLines.addTwoPointRectangle({n.point(0, 0)}, {n.point(plate_w, plate_h)})

        plateProf = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, "plateProf", plate_t, "NewBody", ext)}

        # Slot (rectangle with semicircle ends)
        {sk2} = {root}.sketches.add({ext}.endFaces.item(0))
        slotLines = {sk2}.sketchCurves.sketchLines
        slotLines.addTwoPointRectangle(
            {n.point(round(cx - length/2, 2), round(cy - half_w, 2))},
            {n.point(round(cx + length/2, 2), round(cy + half_w, 2))}
        )

        slotProf = {sk2}.profiles.item(0)
        slotInput = {root}.features.extrudeFeatures.createInput(slotProf, adsk.fusion.FeatureOperations.CutFeatureOperation)
        slotInput.setAllExtent(adsk.fusion.ExtentDirections.NegativeExtentDirection)
        {root}.features.extrudeFeatures.add(slotInput)
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "slot", d)


# ── U-CHANNEL ──────────────────────────────────────────────────

def gen_u_channel(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    w = _dim(rng, 3, 10)
    h = _dim(rng, 3, 10)
    thick = _dim(rng, 0.3, 1.5)
    depth = _dim(rng, 3, 12)
    root = n.root()
    sk = n.sketch("profile")
    prof = n.profile()
    ext = n.extrude("channel")
    plane_expr, _ = _plane(rng, root)
    inner_w = round(w - 2 * thick, 1)
    inner_h = round(h - thick, 1)

    descs = [
        f"{_verb(rng)} a U-channel: {w}cm wide, {h}cm tall, {thick}cm wall, {depth}cm long",
        f"{_verb(rng)} a U-shaped channel profile, {w}x{h}cm cross-section, {thick}cm walls, extruded {depth}cm",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({plane_expr})
        lines = {sk}.sketchCurves.sketchLines

        # Outer U-shape (8 points)
        lines.addByTwoPoints({n.point(0, 0)}, {n.point(w, 0)})
        lines.addByTwoPoints({n.point(w, 0)}, {n.point(w, h)})
        lines.addByTwoPoints({n.point(w, h)}, {n.point(round(w - thick, 1), h)})
        lines.addByTwoPoints({n.point(round(w - thick, 1), h)}, {n.point(round(w - thick, 1), thick)})
        lines.addByTwoPoints({n.point(round(w - thick, 1), thick)}, {n.point(thick, thick)})
        lines.addByTwoPoints({n.point(thick, thick)}, {n.point(thick, h)})
        lines.addByTwoPoints({n.point(thick, h)}, {n.point(0, h)})
        lines.addByTwoPoints({n.point(0, h)}, {n.point(0, 0)})

        {prof} = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, prof, depth, "NewBody", ext)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "u_channel", d)


# ── T-SHAPE ────────────────────────────────────────────────────

def gen_t_shape(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    top_w = _dim(rng, 4, 12)
    top_h = _dim(rng, 0.5, 2)
    stem_w = _dim(rng, 1, top_w * 0.5)
    stem_h = _dim(rng, 3, 10)
    depth = _dim(rng, 2, 8)
    root = n.root()
    sk = n.sketch("profile")
    prof = n.profile()
    ext = n.extrude("tshape")
    plane_expr, _ = _plane(rng, root)
    stem_x = round((top_w - stem_w) / 2, 1)

    descs = [
        f"{_verb(rng)} a T-shape: top {top_w}x{top_h}cm, stem {stem_w}x{stem_h}cm, depth {depth}cm",
        f"{_verb(rng)} a T-shaped extrusion: {top_w}cm wide top, {stem_w}cm wide stem, {depth}cm deep",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({plane_expr})
        lines = {sk}.sketchCurves.sketchLines

        # T-profile
        lines.addByTwoPoints({n.point(0, stem_h)}, {n.point(0, round(stem_h + top_h, 1))})
        lines.addByTwoPoints({n.point(0, round(stem_h + top_h, 1))}, {n.point(top_w, round(stem_h + top_h, 1))})
        lines.addByTwoPoints({n.point(top_w, round(stem_h + top_h, 1))}, {n.point(top_w, stem_h)})
        lines.addByTwoPoints({n.point(top_w, stem_h)}, {n.point(round(stem_x + stem_w, 1), stem_h)})
        lines.addByTwoPoints({n.point(round(stem_x + stem_w, 1), stem_h)}, {n.point(round(stem_x + stem_w, 1), 0)})
        lines.addByTwoPoints({n.point(round(stem_x + stem_w, 1), 0)}, {n.point(stem_x, 0)})
        lines.addByTwoPoints({n.point(stem_x, 0)}, {n.point(stem_x, stem_h)})
        lines.addByTwoPoints({n.point(stem_x, stem_h)}, {n.point(0, stem_h)})

        {prof} = {sk}.profiles.item(0)
{_extrude_code(rng, n, root, prof, depth, "NewBody", ext)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "t_shape", d)


# ── BOOLEAN CUT (cylinder from box) ────────────────────────────

def gen_boolean_cut(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    w, h, depth = _dim(rng, 4, 15), _dim(rng, 4, 15), _dim(rng, 2, 10)
    cut_r = _dim(rng, 0.5, min(w, h) * 0.3)
    root = n.root()
    sk1 = n.sketch("box")
    sk2 = n.sketch("cut")
    ext1 = n.extrude("box")

    descs = [
        f"{_verb(rng)} a {w}x{h}x{depth}cm box with a {cut_r}cm radius cylindrical hole through the center",
        f"{_verb(rng)} a block ({w}x{h}x{depth}cm) with a centered circular cutout (r={cut_r}cm)",
    ]

    cx, cy = round(w / 2, 1), round(h / 2, 1)

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk1} = sketches.add({root}.xYConstructionPlane)
        {sk1}.sketchCurves.sketchLines.addTwoPointRectangle({n.point(0, 0)}, {n.point(w, h)})

        boxProf = {sk1}.profiles.item(0)
{_extrude_code(rng, n, root, "boxProf", depth, "NewBody", ext1)}

        # Cylindrical cut
        {sk2} = sketches.add({ext1}.endFaces.item(0))
        {sk2}.sketchCurves.sketchCircles.addByCenterRadius({n.point(cx, cy)}, {cut_r})
        cutProf = {sk2}.profiles.item(0)
        cutInput = {root}.features.extrudeFeatures.createInput(cutProf, adsk.fusion.FeatureOperations.CutFeatureOperation)
        cutInput.setAllExtent(adsk.fusion.ExtentDirections.NegativeExtentDirection)
        {root}.features.extrudeFeatures.add(cutInput)
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "boolean_cut", d)


# ── HOLLOW CYLINDER (pipe/tube) ─────────────────────────────────

def gen_pipe(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    outer_r = _dim(rng, 2, 10)
    inner_r = _dim(rng, 0.5, outer_r * 0.7)
    height = _dim(rng, 1, 15)
    root = n.root()
    sk = n.sketch("pipe")
    ext = n.extrude("pipe")

    descs = [
        f"{_verb(rng)} a hollow cylinder: outer radius {outer_r}cm, inner radius {inner_r}cm, height {height}cm",
        f"{_verb(rng)} a tube/pipe: OD={round(outer_r*2,1)}cm, ID={round(inner_r*2,1)}cm, length {height}cm",
        f"Pipe: outer r={outer_r}cm, inner r={inner_r}cm, h={height}cm",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches
        {sk} = sketches.add({root}.xYConstructionPlane)
        center = {n.point(0, 0)}
        {sk}.sketchCurves.sketchCircles.addByCenterRadius(center, {outer_r})
        {sk}.sketchCurves.sketchCircles.addByCenterRadius(center, {inner_r})

        # Select the ring profile (between circles)
        ringProf = {sk}.profiles.item(1)
{_extrude_code(rng, n, root, "ringProf", height, "NewBody", ext)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "pipe", d)


# ── STEPPED CYLINDER ───────────────────────────────────────────

def gen_stepped_cylinder(rng: random.Random, d: int) -> CADProblem:
    n = Names(rng)
    r1 = _dim(rng, 3, 8)
    h1 = _dim(rng, 2, 8)
    r2 = _dim(rng, 1, r1 * 0.7)
    h2 = _dim(rng, 1, 6)
    root = n.root()
    sk1 = n.sketch("base")
    sk2 = n.sketch("step")
    ext1 = n.extrude("base")
    ext2 = n.extrude("step")

    descs = [
        f"{_verb(rng)} a stepped cylinder: base r={r1}cm h={h1}cm, step r={r2}cm h={h2}cm",
        f"{_verb(rng)} a two-tier cylinder: bottom {round(r1*2,1)}cm dia × {h1}cm, top {round(r2*2,1)}cm dia × {h2}cm",
    ]

    code = _boilerplate_start(rng, n)
    code += f"""        sketches = {root}.sketches

        # Base cylinder
        {sk1} = sketches.add({root}.xYConstructionPlane)
        {sk1}.sketchCurves.sketchCircles.addByCenterRadius({n.point(0, 0)}, {r1})
        baseProf = {sk1}.profiles.item(0)
{_extrude_code(rng, n, root, "baseProf", h1, "NewBody", ext1)}

        # Stepped top
        {sk2} = sketches.add({ext1}.endFaces.item(0))
        {sk2}.sketchCurves.sketchCircles.addByCenterRadius({n.point(0, 0)}, {r2})
        stepProf = {sk2}.profiles.item(0)
{_extrude_code(rng, n, root, "stepProf", h2, "Join", ext2)}
"""
    code += _boilerplate_end(rng)
    return CADProblem(rng.choice(descs), code, "stepped_cylinder", d)


# ── Generator Registry ──────────────────────────────────────────

GENERATORS = [
    # Level 1: Basic primitives
    (gen_box, 12, 1),
    (gen_cylinder, 12, 1),
    (gen_sphere, 8, 1),
    (gen_cone, 6, 1),
    (gen_wedge, 6, 1),
    (gen_polygon_prism, 6, 1),
    (gen_torus, 5, 1),
    # Level 2: Single feature shapes
    (gen_pipe, 8, 2),
    (gen_l_bracket, 7, 2),
    (gen_u_channel, 7, 2),
    (gen_t_shape, 6, 2),
    (gen_slot, 6, 2),
    (gen_stepped_cylinder, 6, 2),
    (gen_boolean_cut, 6, 2),
    # Level 3: Compound (base + random features)
    (gen_plate_with_holes, 8, 3),
    (gen_circular_pattern, 6, 3),
    (gen_compound, 15, 3),
]

_GEN_FUNCS = [fn for fn, _, _ in GENERATORS]
_GEN_WEIGHTS = [w for _, w, _ in GENERATORS]
_GEN_LEVELS = [lvl for _, _, lvl in GENERATORS]


def generate_cad_problem(rng: random.Random, difficulty: int = 0) -> CADProblem:
    if difficulty == 0:
        fn = rng.choices(_GEN_FUNCS, weights=_GEN_WEIGHTS)[0]
    else:
        eligible = [(fn, w) for fn, w, lvl in GENERATORS if lvl <= difficulty]
        if not eligible:
            eligible = list(zip(_GEN_FUNCS, _GEN_WEIGHTS))
        fns, wts = zip(*eligible)
        fn = rng.choices(fns, weights=wts)[0]
    return fn(rng, difficulty or 2)


# ── Chat Formatting ─────────────────────────────────────────────

SYSTEM_VARIANTS = [
    "You are a Fusion 360 Python API expert. Generate complete, runnable scripts. Output ONLY the Python code.",
    "Write Fusion 360 Python API code to create the described 3D model. Output only the code.",
    "You generate Fusion 360 automation scripts. Respond with complete Python code only.",
    "Create a Fusion 360 Python script for the requested 3D shape. Code only, no explanation.",
    "You are a CAD automation assistant. Write Fusion 360 Python API scripts. Only output code.",
    "Generate a Fusion 360 add-in script that creates the specified geometry. Code only.",
]


def problem_to_chat(problem: CADProblem, rng: random.Random) -> dict:
    return {
        "messages": [
            {"role": "system", "content": rng.choice(SYSTEM_VARIANTS)},
            {"role": "user", "content": problem.description},
            {"role": "assistant", "content": problem.code},
        ],
        "metadata": {
            "operation": problem.operation,
            "difficulty": problem.difficulty,
            "features": problem.features,
        },
    }


def format_chat_text(example: dict) -> dict:
    parts = [f"<|im_start|>{m['role']}\n{m['content']}<|im_end|>"
             for m in example["messages"]]
    return {"text": "\n".join(parts)}


# ── Streaming Dataset ───────────────────────────────────────────

class Fusion360StreamDataset(IterableDataset):
    def __init__(self, difficulty: int = 0, seed: int = 42):
        self.difficulty = difficulty
        self.base_seed = seed

    def __iter__(self) -> Iterator[dict]:
        worker_info = torch.utils.data.get_worker_info()
        seed = self.base_seed + (worker_info.id * 1_000_000 if worker_info else 0)
        rng = random.Random(seed)
        counter = 0
        while True:
            if counter % 100_000 == 0 and counter > 0:
                rng = random.Random(seed + counter)
            problem = generate_cad_problem(rng, self.difficulty)
            chat = problem_to_chat(problem, rng)
            formatted = format_chat_text(chat)
            formatted["metadata"] = chat["metadata"]
            yield formatted
            counter += 1


# ── Fixed Eval Set ──────────────────────────────────────────────

def generate_fusion_eval_set(n: int = 200, seed: int = 9999) -> list[dict]:
    rng = random.Random(seed)
    problems = []
    for _ in range(n):
        problem = generate_cad_problem(rng, difficulty=0)
        chat = problem_to_chat(problem, rng)
        formatted = format_chat_text(chat)
        formatted["metadata"] = chat["metadata"]
        formatted["description"] = problem.description
        formatted["expected_code"] = problem.code
        formatted["operation"] = problem.operation
        formatted["messages"] = chat["messages"]
        problems.append(formatted)
    rng.shuffle(problems)
    return problems


# ── CLI ─────────────────────────────────────────────────────────

def main():
    parser = argparse.ArgumentParser(description="Fusion 360 Code Generator v2")
    parser.add_argument("--preview", type=int, default=0)
    parser.add_argument("--preview-code", type=int, default=0)
    parser.add_argument("--generate-eval", type=int, default=0)
    parser.add_argument("--output", type=str, default="eval_fusion360.jsonl")
    parser.add_argument("--difficulty", type=int, default=0)
    parser.add_argument("--seed", type=int, default=42)
    parser.add_argument("--stats", action="store_true")
    args = parser.parse_args()

    if args.preview > 0:
        rng = random.Random(args.seed)
        print(f"=== {args.preview} Fusion 360 Problems ===\n")
        for _ in range(args.preview):
            p = generate_cad_problem(rng, args.difficulty)
            feats = f" [{', '.join(p.features)}]" if p.features else ""
            print(f"  [{p.operation:>18} d={p.difficulty}{feats}]")
            print(f"  {p.description}\n")

    if args.preview_code > 0:
        rng = random.Random(args.seed)
        for _ in range(args.preview_code):
            p = generate_cad_problem(rng, args.difficulty)
            print(f"{'='*60}")
            print(f"Desc: {p.description}")
            print(f"Op: {p.operation} | D: {p.difficulty} | Feats: {p.features}")
            print(f"{'='*60}")
            print(p.code)
            print()

    if args.generate_eval > 0:
        ev = generate_fusion_eval_set(args.generate_eval, args.seed)
        with open(args.output, "w") as f:
            for item in ev:
                f.write(json.dumps(item) + "\n")
        print(f"Wrote {len(ev)} eval problems to {args.output}")

    if args.stats:
        rng = random.Random(args.seed)
        from collections import Counter
        ops = Counter()
        for _ in range(5000):
            p = generate_cad_problem(rng, args.difficulty)
            ops[p.operation] += 1
        print("\n=== Operation Distribution (5K samples) ===")
        for op, count in ops.most_common():
            print(f"  {op:>18}: {count:>5} ({count/50:.1f}%)")


if __name__ == "__main__":
    main()

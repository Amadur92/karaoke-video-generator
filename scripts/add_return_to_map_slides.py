#!/usr/bin/env python3
from __future__ import annotations

import argparse
import re
import shutil
import zipfile
from pathlib import Path

from lxml import etree


NS = {
    "p": "http://schemas.openxmlformats.org/presentationml/2006/main",
    "r": "http://schemas.openxmlformats.org/officeDocument/2006/relationships",
    "rel": "http://schemas.openxmlformats.org/package/2006/relationships",
    "ct": "http://schemas.openxmlformats.org/package/2006/content-types",
}

for prefix in ("p", "r"):
    etree.register_namespace(prefix, NS[prefix])


SLIDE_REL_TYPE = "http://schemas.openxmlformats.org/officeDocument/2006/relationships/slide"
SLIDE_CONTENT_TYPE = "application/vnd.openxmlformats-officedocument.presentationml.slide+xml"


def q(ns: str, tag: str) -> str:
    return f"{{{NS[ns]}}}{tag}"


def serialize(root: etree._Element) -> bytes:
    return etree.tostring(root, xml_declaration=True, encoding="UTF-8", standalone=True)


def relationship_number(rel_id: str) -> int:
    match = re.fullmatch(r"rId(\d+)", rel_id)
    return int(match.group(1)) if match else 0


def build(input_pptx: Path, output_pptx: Path, map_slide: int) -> None:
    output_pptx.parent.mkdir(parents=True, exist_ok=True)

    with zipfile.ZipFile(input_pptx, "r") as zin:
        names = set(zin.namelist())
        map_slide_path = f"ppt/slides/slide{map_slide}.xml"
        map_rels_path = f"ppt/slides/_rels/slide{map_slide}.xml.rels"
        if map_slide_path not in names or map_rels_path not in names:
            raise FileNotFoundError(f"Map slide {map_slide} is not complete in {input_pptx}")

        presentation = etree.fromstring(zin.read("ppt/presentation.xml"))
        presentation_rels = etree.fromstring(zin.read("ppt/_rels/presentation.xml.rels"))
        content_types = etree.fromstring(zin.read("[Content_Types].xml"))

        slide_id_list = presentation.xpath("//p:sldIdLst", namespaces=NS)[0]
        original_slide_ids = list(slide_id_list.xpath("./p:sldId", namespaces=NS))
        max_slide_id = max(int(node.get("id")) for node in original_slide_ids)

        existing_slide_numbers = []
        for name in names:
            match = re.fullmatch(r"ppt/slides/slide(\d+)\.xml", name)
            if match:
                existing_slide_numbers.append(int(match.group(1)))
        next_slide_number = max(existing_slide_numbers) + 1

        max_rel_number = max(
            relationship_number(rel.get("Id", ""))
            for rel in presentation_rels.xpath("//rel:Relationship", namespaces=NS)
        )
        next_rel_number = max_rel_number + 1

        new_slide_parts: dict[str, bytes] = {}
        new_rels_parts: dict[str, bytes] = {}

        for original in original_slide_ids:
            clone_slide_number = next_slide_number
            clone_slide_path = f"ppt/slides/slide{clone_slide_number}.xml"
            clone_rels_path = f"ppt/slides/_rels/slide{clone_slide_number}.xml.rels"
            new_slide_parts[clone_slide_path] = zin.read(map_slide_path)
            new_rels_parts[clone_rels_path] = zin.read(map_rels_path)

            rel_id = f"rId{next_rel_number}"
            etree.SubElement(
                presentation_rels,
                q("rel", "Relationship"),
                Id=rel_id,
                Type=SLIDE_REL_TYPE,
                Target=f"slides/slide{clone_slide_number}.xml",
            )
            clone_id = etree.Element(q("p", "sldId"), id=str(max_slide_id + 1))
            clone_id.set(q("r", "id"), rel_id)
            original.addnext(clone_id)

            etree.SubElement(
                content_types,
                q("ct", "Override"),
                PartName=f"/ppt/slides/slide{clone_slide_number}.xml",
                ContentType=SLIDE_CONTENT_TYPE,
            )

            max_slide_id += 1
            next_slide_number += 1
            next_rel_number += 1

        modified = {
            "ppt/presentation.xml": serialize(presentation),
            "ppt/_rels/presentation.xml.rels": serialize(presentation_rels),
            "[Content_Types].xml": serialize(content_types),
        }

        if "docProps/app.xml" in names:
            app = etree.fromstring(zin.read("docProps/app.xml"))
            for node in app.xpath("//*[local-name()='Slides']"):
                node.text = str(len(original_slide_ids) * 2)
            modified["docProps/app.xml"] = serialize(app)

        tmp_output = output_pptx.with_suffix(output_pptx.suffix + ".tmp")
        if tmp_output.exists():
            tmp_output.unlink()
        with zipfile.ZipFile(tmp_output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=1, allowZip64=True) as zout:
            for info in zin.infolist():
                if info.filename in modified:
                    zout.writestr(info, modified[info.filename])
                else:
                    with zin.open(info, "r") as source, zout.open(info, "w") as target:
                        shutil.copyfileobj(source, target, length=1024 * 1024)
            for path, data in new_slide_parts.items():
                zout.writestr(path, data)
            for path, data in new_rels_parts.items():
                zout.writestr(path, data)

        tmp_output.replace(output_pptx)
        print(f"[pptx] wrote {output_pptx}")
        print(f"[pptx] slides {len(original_slide_ids)} -> {len(original_slide_ids) * 2}")


def main() -> None:
    parser = argparse.ArgumentParser(description="Insert map-slide clones after every slide so Next returns to the map.")
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--map-slide", type=int, default=21)
    args = parser.parse_args()
    build(args.input, args.output, args.map_slide)


if __name__ == "__main__":
    main()

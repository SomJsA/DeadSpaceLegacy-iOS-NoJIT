"""Fetch fixed public dependency sources without uploading local copies."""
from pathlib import Path, PurePosixPath
import io
import shutil
import urllib.request
import zipfile

root=Path(__file__).resolve().parents[2]
dependencies=[
 ('libsdl-org/SDL','07d0f51fa292895443f563f0cbde4cb3802d87fa','vendor/rust-sdl2/sdl2-sys/SDL'),
 ('GNOME/libxml2','02d577715e1951cf114ca4e9adaf5605156c4f4c','vendor/libxml2'),
 ('kcat/openal-soft','23c8a35505fe6ab7a5c87754911a133b23ac75cf','vendor/openal-soft'),
 ('nothings/stb','9f1776a36d2a3d63c52f705c3a84b372cfed4340','vendor/stb'),
]
for repo,commit,relative in dependencies:
    destination=root/relative
    if destination.exists():
        raise SystemExit('Refusing to replace dependency directory: '+str(destination))
    url='https://codeload.github.com/'+repo+'/zip/'+commit
    with urllib.request.urlopen(url,timeout=120) as response:
        data=response.read()
    with zipfile.ZipFile(io.BytesIO(data)) as archive:
        assert archive.testzip() is None
        for item in archive.infolist():
            path=PurePosixPath(item.filename)
            assert not path.is_absolute() and '..' not in path.parts
            # GitHub archives contain one wrapper directory.
            stripped=path.parts[1:]
            if not stripped:continue
            target=destination.joinpath(*stripped)
            assert target.resolve().is_relative_to(destination.resolve())
            if item.is_dir():target.mkdir(parents=True,exist_ok=True)
            else:
                target.parent.mkdir(parents=True,exist_ok=True)
                with archive.open(item) as source,target.open('wb') as output:
                    shutil.copyfileobj(source,output)
    print(repo,commit,'ready',flush=True)

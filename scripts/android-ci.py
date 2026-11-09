"""Prépare le projet Android généré par `tauri android init` pour une version publiée.

Lancé par .github/workflows/android.yml, depuis la racine du dépôt :
  python3 scripts/android-ci.py <dossier des icônes générées> <keystore.jks> <mot de passe>

- icônes : celles de src-tauri/icons/icon-android.svg (même logo que sur PC), fond sombre ;
- signature : la clé de GoRide, pour que chaque version s'installe par-dessus la précédente.
"""
import pathlib
import shutil
import sys

ANDROID = pathlib.Path("src-tauri/gen/android")
RES = ANDROID / "app/src/main/res"

icons, keystore, password = sys.argv[1], sys.argv[2], sys.argv[3]

# 1. Icônes
for d in pathlib.Path(icons, "android").iterdir():
    target = RES / d.name
    target.mkdir(parents=True, exist_ok=True)
    for f in d.iterdir():
        shutil.copy(f, target / f.name)
bg = RES / "values/ic_launcher_background.xml"
bg.write_text(bg.read_text().replace("#FFFFFF", "#14181F"))

# 2. Signature
shutil.copy(keystore, ANDROID / "goride.jks")
(ANDROID / "keystore.properties").write_text(
    f"storeFile=../goride.jks\nkeyAlias=goride\npassword={password}\n"
)
gradle = ANDROID / "app/build.gradle.kts"
s = gradle.read_text()
if "signingConfigs" not in s:
    s = s.replace("import java.util.Properties", "import java.util.Properties\nimport java.io.FileInputStream", 1)
    signing = '''    signingConfigs {
        create("release") {
            val props = Properties()
            props.load(FileInputStream(rootProject.file("keystore.properties")))
            storeFile = file(props["storeFile"] as String)
            keyAlias = props["keyAlias"] as String
            storePassword = props["password"] as String
            keyPassword = props["password"] as String
        }
    }
    buildTypes {'''
    assert "    buildTypes {" in s, "build.gradle.kts : bloc buildTypes introuvable"
    s = s.replace("    buildTypes {", signing, 1)
    assert 'getByName("release") {' in s, "build.gradle.kts : type release introuvable"
    s = s.replace(
        'getByName("release") {',
        'getByName("release") {\n            signingConfig = signingConfigs.getByName("release")',
        1,
    )
    gradle.write_text(s)
print("Projet Android prêt : icônes GoRide et signature.")

import java.util.Properties
import java.util.zip.ZipFile
import org.gradle.api.tasks.testing.Test
import org.gradle.api.artifacts.result.ResolvedArtifactResult
import org.gradle.maven.MavenModule
import org.gradle.maven.MavenPomArtifact
import javax.xml.parsers.DocumentBuilderFactory

// AGP 9's built-in Kotlin support means the separate org.jetbrains.kotlin.android
// plugin must not be applied (AGP rejects it outright) — only compiler plugins
// (Compose, serialization) are still applied explicitly.
plugins {
    alias(libs.plugins.android.application)
    alias(libs.plugins.kotlin.compose)
    alias(libs.plugins.kotlin.serialization)
    alias(libs.plugins.google.services)
}

val localReleaseProperties = Properties().apply {
    val source = rootProject.file("key.properties")
    if (source.isFile) source.inputStream().use(::load)
}

fun releaseValue(name: String): String? = providers.environmentVariable(name).orNull
    ?: providers.gradleProperty(name).orNull
    ?: localReleaseProperties.getProperty(name)

val releaseVersionName = releaseValue("CHOOSH_VERSION_NAME") ?: "0.0.0"
val releaseVersionCode = releaseValue("CHOOSH_VERSION_CODE")?.toIntOrNull() ?: 1
val previewCodename = providers.gradleProperty("choosh.previewCodename").orNull
if (previewCodename != null) {
    require(previewCodename.matches(Regex("[A-Za-z0-9]+"))) {
        "choosh.previewCodename must contain only ASCII letters and digits"
    }
}
val signingNames = listOf(
    "CHOOSH_KEYSTORE_FILE",
    "CHOOSH_KEYSTORE_PASSWORD",
    "CHOOSH_KEY_ALIAS",
    "CHOOSH_KEY_PASSWORD",
)

android {
    namespace = "ai.choosh"
    if (previewCodename == null) {
        // Stable CI deliberately lints and ships against the supported API 36 baseline.
        compileSdk = 36
    } else {
        compileSdkPreview = previewCodename
    }

    defaultConfig {
        applicationId = "ai.choosh"
        minSdk = 26
        if (previewCodename == null) {
            targetSdk = 36
        } else {
            targetSdkPreview = previewCodename
        }
        versionCode = releaseVersionCode
        versionName = releaseVersionName
        // No server-discovery mechanism exists yet (a later increment); the relay
        // URL is a build-time constant until then.
        buildConfigField(
            "String",
            "CHOOSH_RELAYD_URL",
            "\"${releaseValue("CHOOSH_RELAYD_URL") ?: "wss://relay.choosh.ai/connect"}\"",
        )
        // relayd's WebAuthn ceremony endpoints (/webauthn/register/*,
        // /webauthn/login/*) are plain HTTPS at the relay host's root, not
        // under the WS /connect path above — a separate constant rather
        // than deriving it via string surgery on CHOOSH_RELAYD_URL.
        buildConfigField(
            "String",
            "CHOOSH_RELAYD_HTTP_URL",
            "\"${releaseValue("CHOOSH_RELAYD_HTTP_URL") ?: "https://relay.choosh.ai"}\"",
        )
        // The bespoke SSH-era SmokeInstrumentation runner is gone along with the code it
        // exercised; instrumented tests (once this sandbox has an attached device/emulator
        // to run them on) use the standard AndroidX runner.
        testInstrumentationRunner = "androidx.test.runner.AndroidJUnitRunner"
    }
    buildToolsVersion = "36.0.0"
    signingConfigs {
        create("chooshRelease") {
            storeFile = file(releaseValue("CHOOSH_KEYSTORE_FILE") ?: "missing-release-keystore")
            storePassword = releaseValue("CHOOSH_KEYSTORE_PASSWORD")
            keyAlias = releaseValue("CHOOSH_KEY_ALIAS")
            keyPassword = releaseValue("CHOOSH_KEY_PASSWORD")
        }
    }
    buildTypes {
        release {
            isMinifyEnabled = false
            signingConfig = signingConfigs.getByName("chooshRelease")
        }
    }
    compileOptions {
        sourceCompatibility = JavaVersion.VERSION_17
        targetCompatibility = JavaVersion.VERSION_17
    }
    buildFeatures {
        compose = true
        buildConfig = true
    }
    testOptions { unitTests.isIncludeAndroidResources = false }
    lint {
        abortOnError = true
        warningsAsErrors = true
        checkDependencies = true
        // Android Lint's BidirectionalText detector currently crashes while
        // parsing JavaDoc in the pinned toolchain; retain all other checks.
        disable += "BidiSpoofing"
    }
}

dependencies {
    implementation(platform(libs.composeBom))
    androidTestImplementation(platform(libs.composeBom))
    implementation(platform(libs.firebaseBom))

    implementation(libs.soraEditor)
    implementation(libs.firebaseMessaging)
    implementation(libs.composeUi)
    implementation(libs.composeUiToolingPreview)
    implementation(libs.composeMaterial3)
    implementation(libs.composeMaterialIconsExtended)
    implementation(libs.activityCompose)
    implementation(libs.lifecycleViewmodelCompose)
    implementation(libs.lifecycleRuntimeKtx)
    implementation(libs.lifecycleRuntimeCompose)
    implementation(libs.credentials)
    implementation(libs.credentialsPlayServicesAuth)
    implementation(libs.securityCrypto)
    implementation(libs.customview)
    implementation(libs.kotlinxCoroutinesAndroid)
    implementation(libs.kotlinxCoroutinesPlayServices)
    implementation(libs.kotlinxSerializationJson)
    debugImplementation(libs.composeUiTooling)
    debugImplementation(libs.composeUiTestManifest)

    testImplementation(libs.junit4)
    testImplementation(libs.kotlinxCoroutinesTest)

    androidTestImplementation(libs.androidxTestRunner)
    androidTestImplementation(libs.androidxTestExtJunit)
    androidTestImplementation(libs.espressoCore)
    androidTestImplementation(libs.composeUiTestJunit4)
}

dependencyLocking { lockAllConfigurations() }

// JVM tests may inspect checked-in Android fixtures. Make their module-relative paths
// deterministic instead of depending on Gradle's invocation directory.
tasks.withType<Test>().configureEach {
    workingDir = project.projectDir
}

val buildRustAndroid = tasks.register<Exec>("buildRustAndroid") {
    group = "build"
    description = "Build and ABI-check the Rust bridge for arm64-v8a and x86_64."
    workingDir(rootProject.projectDir)
    commandLine(rootProject.file("scripts/build-android-rust.sh").absolutePath)
}

tasks.matching { it.name == "testDebugUnitTest" }.configureEach {
    dependsOn(buildRustAndroid)
}

val checkNativeAbiPackaging = tasks.register("checkNativeAbiPackaging") {
    group = "verification"
    description = "Builds and verifies both native bridge ABIs in the debug APK."
    dependsOn(tasks.named("assembleDebug"))
    val apk = layout.buildDirectory.file("outputs/apk/debug/app-debug.apk")
    inputs.file(apk)
    doLast {
        val apkFile = apk.get().asFile
        check(apkFile.isFile) { "Debug APK was not created: ${apkFile.path}" }
        val required = setOf("arm64-v8a", "x86_64")
        val names = ZipFile(apkFile).use { archive ->
            required.associateWith { abi ->
                archive.entries().asSequence()
                    .map { it.name }
                    .filter { it.startsWith("lib/$abi/") && it.endsWith(".so") }
                    .map { it.removePrefix("lib/$abi/") }
                    .sorted()
                    .toList()
            }
        }
        check(names.values.all { it.isNotEmpty() }) { "Debug APK must package arm64-v8a and x86_64 native libraries" }
        check(names.getValue("arm64-v8a") == names.getValue("x86_64")) {
            "Native bridge library sets differ between required ABIs"
        }
    }
}

tasks.matching { it.name == "assembleDebug" || it.name == "assembleRelease" }.configureEach {
    dependsOn(buildRustAndroid)
}

// `assembleDebug dependsOn buildRustAndroid` alone is not sufficient
// ordering: it only guarantees `buildRustAndroid` finishes before
// `assembleDebug` itself runs, not before the packaging tasks that read
// `jniLibs/` along the way (`mergeDebugJniLibFolders`, `mergeDebugNativeLibs`,
// and their Release equivalents) — those are siblings of `buildRustAndroid`
// in the task graph, not descendants of it, so Gradle was free to schedule
// them first and package a stale `.so` (confirmed on a real device: an
// incremental Rust-only change rebuilt the library correctly, but the
// installed APK's behavior didn't change because these merge tasks had
// already read the previous `jniLibs/` contents and reported themselves
// up-to-date before `buildRustAndroid` overwrote them). Every task that
// reads `jniLibs/` must depend on `buildRustAndroid` directly.
tasks.matching { it.name.contains("JniLibFolders") || it.name.contains("NativeLibs") }.configureEach {
    dependsOn(buildRustAndroid)
}

val validateReleaseEvidence = tasks.register("validateReleaseEvidence") {
    group = "verification"
    doLast {
        val missing = signingNames.filter { releaseValue(it).isNullOrBlank() }
        check(missing.isEmpty()) { "Release signing is incomplete; missing: ${missing.joinToString()}" }
        check(releaseVersionName.matches(Regex("(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)\\.(0|[1-9][0-9]*)"))) {
            "CHOOSH_VERSION_NAME must be MAJOR.MINOR.PATCH"
        }
        check(releaseVersionCode in 1..2_100_000_000) { "CHOOSH_VERSION_CODE is outside Android bounds" }
        check(file(requireNotNull(releaseValue("CHOOSH_KEYSTORE_FILE"))).isFile) { "Release keystore is not a file" }
    }
}

tasks.matching { it.name == "assembleRelease" || it.name == "packageRelease" }.configureEach {
    dependsOn(validateReleaseEvidence)
}

fun json(value: String): String = buildString {
    append('"')
    value.forEach { character ->
        when (character) {
            '"' -> append("\\\"")
            '\\' -> append("\\\\")
            else -> append(character)
        }
    }
    append('"')
}

val cyclonedxBom = tasks.register("cyclonedxBom") {
    group = "reporting"
    val output = layout.buildDirectory.file("reports/bom.json")
    outputs.file(output)
    doLast {
        val components = configurations.getByName("releaseRuntimeClasspath").resolvedConfiguration
            .resolvedArtifacts.map { artifact ->
                Triple(artifact.moduleVersion.id.group, artifact.name, artifact.moduleVersion.id.version)
            }.distinct().sortedWith(compareBy({ it.first }, { it.second }, { it.third }))
        val body = components.joinToString(",") { (group, name, version) ->
            "{\"type\":\"library\",\"group\":${json(group)},\"name\":${json(name)},\"version\":${json(version)},\"purl\":${json("pkg:maven/$group/$name@$version")}}"
        }
        output.get().asFile.apply {
            parentFile.mkdirs()
            writeText("{\"bomFormat\":\"CycloneDX\",\"specVersion\":\"1.6\",\"serialNumber\":\"urn:uuid:00000000-0000-0000-0000-000000000000\",\"version\":1,\"components\":[$body]}\n")
        }
    }
}

// A real, LGPL-relevant NOTICE.txt: not just a coordinate list. Resolves each
// releaseRuntimeClasspath dependency's *actual published Maven POM licence
// declaration* (an ArtifactResolutionQuery for MavenPomArtifact — the same
// technique dedicated license-report Gradle plugins use), buckets
// dependencies by their declared licence, and embeds the complete licence
// text for the two licences that cover the overwhelming majority of the
// graph (Apache-2.0: nearly all of AndroidX/Kotlin/Firebase/Gson; LGPL-2.1:
// Sora Editor, the one dependency this matters legally for — see
// docs/licenses/sora-packaging.md). Anything the resolved POM doesn't
// declare a machine-readable <licenses> entry for (this is normal for
// Google Play Services artifacts, which ship under the Android SDK terms
// rather than an OSI licence) is called out explicitly rather than silently
// mislabeled as Apache-2.0 or dropped.
val generateReleaseLicenseReport = tasks.register("generateReleaseLicenseReport") {
    group = "reporting"
    val output = layout.buildDirectory.file("reports/licenses/NOTICE.txt")
    val apache2File = file("licenses/Apache-2.0.txt")
    val lgpl21File = file("licenses/LGPL-2.1.txt")
    outputs.file(output)
    inputs.file(apache2File)
    inputs.file(lgpl21File)
    doLast {
        data class Coord(val group: String, val name: String, val version: String) : Comparable<Coord> {
            override fun compareTo(other: Coord) =
                compareValuesBy(this, other, { it.group }, { it.name }, { it.version })
            override fun toString() = "$group:$name:$version"
        }
        data class LicenseInfo(val name: String?, val url: String?)

        val resolvedArtifacts = configurations.getByName("releaseRuntimeClasspath").resolvedConfiguration.resolvedArtifacts
        val coordsById = resolvedArtifacts.mapNotNull { artifact ->
            val id = artifact.id.componentIdentifier as? org.gradle.api.artifacts.component.ModuleComponentIdentifier
                ?: return@mapNotNull null
            id to Coord(id.group, id.module, id.version)
        }.toMap()

        val pomQuery = dependencies.createArtifactResolutionQuery()
            .forComponents(coordsById.keys)
            .withArtifacts(MavenModule::class.java, MavenPomArtifact::class.java)
            .execute()

        val builderFactory = DocumentBuilderFactory.newInstance()
        fun parsePomLicense(pomFile: java.io.File): LicenseInfo = try {
            val doc = builderFactory.newDocumentBuilder().parse(pomFile)
            val licenseNodes = doc.getElementsByTagName("license")
            if (licenseNodes.length == 0) {
                LicenseInfo(null, null)
            } else {
                val el = licenseNodes.item(0) as org.w3c.dom.Element
                val name = (el.getElementsByTagName("name").item(0) as? org.w3c.dom.Element)?.textContent?.trim()
                val url = (el.getElementsByTagName("url").item(0) as? org.w3c.dom.Element)?.textContent?.trim()
                LicenseInfo(name, url)
            }
        } catch (e: Exception) {
            LicenseInfo(null, null)
        }

        val licenseByCoord = sortedMapOf<Coord, LicenseInfo>()
        for (component in pomQuery.resolvedComponents) {
            val coord = coordsById[component.id] ?: continue
            var info = LicenseInfo(null, null)
            for (artifact in component.getArtifacts(MavenPomArtifact::class.java)) {
                if (artifact is ResolvedArtifactResult) {
                    info = parsePomLicense(artifact.file)
                    if (info.name != null) break
                }
            }
            licenseByCoord[coord] = info
        }
        // A component the query couldn't resolve at all still gets listed, as undeclared
        // rather than silently omitted.
        for (coord in coordsById.values) licenseByCoord.putIfAbsent(coord, LicenseInfo(null, null))

        fun bucketOf(info: LicenseInfo): String {
            val n = info.name?.lowercase() ?: return "UNDECLARED"
            return when {
                "lgpl" in n || "lesser general public" in n -> "LGPL-2.1-or-later"
                "apache" in n && "2" in n -> "Apache-2.0"
                else -> info.name
            }
        }

        val buckets = sortedMapOf<String, MutableList<Coord>>()
        for ((coord, info) in licenseByCoord) buckets.getOrPut(bucketOf(info)) { mutableListOf() }.add(coord)
        buckets.values.forEach { it.sortWith(compareBy({ c -> c.group }, { c -> c.name }, { c -> c.version })) }

        val text = buildString {
            appendLine("Choosh dependency licence notices")
            appendLine("Generated by :app:generateReleaseLicenseReport from the resolved releaseRuntimeClasspath's")
            appendLine("published Maven POM licence declarations.")
            appendLine("${licenseByCoord.size} distinct runtime dependencies across ${buckets.size} licence bucket(s).")
            appendLine()
            appendLine("=".repeat(78))
            appendLine("SORA EDITOR -- LGPL-2.1-or-later (see docs/licenses/sora-packaging.md)")
            appendLine("=".repeat(78))
            appendLine()
            appendLine("Choosh depends on io.github.rosemoe:editor (Sora Editor) as an unmodified,")
            appendLine("ordinary Maven AAR dependency. Choosh has made NO source or bytecode")
            appendLine("modifications to this library.")
            appendLine()
            appendLine("  Coordinate:         io.github.rosemoe:editor:0.24.6")
            appendLine("  Upstream source:    https://github.com/Rosemoe/sora-editor")
            appendLine("  Source tag/commit:  0.24.6 / 87055c459e346cac6c619b3350f0bfba076228cc")
            appendLine("  Copyright:          Copyright (C) 2020-2024 Rosemoe")
            appendLine("  Declared licence:   \"LGPL v2.1\" (this artifact's published Maven POM <licenses> block)")
            appendLine()
            appendLine("Full licence text follows:")
            appendLine()
            append(lgpl21File.readText())
            appendLine()
            appendLine("=".repeat(78))
            appendLine("APACHE LICENSE, VERSION 2.0 -- applies to the dependencies listed below")
            appendLine("=".repeat(78))
            appendLine()
            append(apache2File.readText())
            appendLine()
            for (bucketName in listOf("LGPL-2.1-or-later", "Apache-2.0")) {
                val coords = buckets[bucketName].orEmpty()
                appendLine("-".repeat(78))
                appendLine("$bucketName -- ${coords.size} dependencies:")
                coords.forEach { appendLine("  $it") }
                appendLine()
            }
            val other = buckets.filterKeys { it != "LGPL-2.1-or-later" && it != "Apache-2.0" && it != "UNDECLARED" }
            if (other.isNotEmpty()) {
                appendLine("=".repeat(78))
                appendLine("OTHER DECLARED LICENCES")
                appendLine("=".repeat(78))
                for ((bucketName, coords) in other) {
                    appendLine()
                    appendLine("$bucketName -- ${coords.size} dependencies:")
                    coords.forEach { c ->
                        val url = licenseByCoord[c]?.url
                        appendLine("  $c" + if (url != null) " ($url)" else "")
                    }
                }
                appendLine()
            }
            val undeclared = buckets["UNDECLARED"].orEmpty()
            if (undeclared.isNotEmpty()) {
                appendLine("=".repeat(78))
                appendLine("UNDECLARED IN RESOLVED POM METADATA -- MANUAL REVIEW REQUIRED")
                appendLine("=".repeat(78))
                appendLine("These artifacts' published Maven POM did not declare a machine-readable")
                appendLine("<licenses> element. Commonly this means a Google Play Services/Firebase")
                appendLine("artifact distributed under the Android Software Development Kit License")
                appendLine("Agreement (https://developer.android.com/studio/terms) rather than an OSI")
                appendLine("licence -- verify the applicable terms for each before treating this list")
                appendLine("as closed.")
                appendLine()
                undeclared.forEach { appendLine("  $it") }
                appendLine()
            }
        }
        output.get().asFile.apply {
            parentFile.mkdirs()
            writeText(text)
        }
    }
}

import java.util.Properties
import java.util.zip.ZipFile

plugins { alias(libs.plugins.android.application) }

val localReleaseProperties = Properties().apply {
    val source = rootProject.file("key.properties")
    if (source.isFile) source.inputStream().use(::load)
}

fun releaseValue(name: String): String? = providers.environmentVariable(name).orNull
    ?: providers.gradleProperty(name).orNull
    ?: localReleaseProperties.getProperty(name)

val releaseVersionName = releaseValue("CHOOSH_VERSION_NAME") ?: "0.0.0"
val releaseVersionCode = releaseValue("CHOOSH_VERSION_CODE")?.toIntOrNull() ?: 1
val signingNames = listOf(
    "CHOOSH_KEYSTORE_FILE",
    "CHOOSH_KEYSTORE_PASSWORD",
    "CHOOSH_KEY_ALIAS",
    "CHOOSH_KEY_PASSWORD",
)

android {
    namespace = "ai.choosh"
    compileSdk = 37

    defaultConfig {
        applicationId = "ai.choosh"
        minSdk = 26
        targetSdk = 37
        versionCode = releaseVersionCode
        versionName = releaseVersionName
        testInstrumentationRunner = "ai.choosh.SmokeInstrumentation"
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
    testOptions { unitTests.isIncludeAndroidResources = false }
    lint {
        abortOnError = true
        warningsAsErrors = true
        checkDependencies = true
    }
}

dependencies {
    implementation(libs.soraEditor)
    testImplementation(libs.junit4)
}

dependencyLocking { lockAllConfigurations() }

val buildRustAndroid = tasks.register<Exec>("buildRustAndroid") {
    group = "build"
    description = "Build and ABI-check the Rust bridge for arm64-v8a and x86_64."
    workingDir(rootProject.projectDir)
    commandLine(rootProject.file("scripts/build-android-rust.sh").absolutePath)
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

val generateReleaseLicenseReport = tasks.register("generateReleaseLicenseReport") {
    group = "reporting"
    val output = layout.buildDirectory.file("reports/licenses/NOTICE.txt")
    outputs.file(output)
    doLast {
        val modules = configurations.getByName("releaseRuntimeClasspath").resolvedConfiguration
            .resolvedArtifacts.map { "${it.moduleVersion.id.group}:${it.name}:${it.moduleVersion.id.version}" }
            .distinct().sorted()
        output.get().asFile.apply {
            parentFile.mkdirs()
            writeText("Choosh dependency coordinates\n\n" + modules.joinToString("\n", postfix = "\n"))
        }
    }
}

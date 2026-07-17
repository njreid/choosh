import java.util.Properties

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
    compileSdk = 36

    defaultConfig {
        applicationId = "ai.choosh"
        minSdk = 26
        targetSdk = 36
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
}

dependencies { testImplementation(libs.junit4) }

val buildRustAndroid by tasks.registering(Exec::class) {
    group = "build"
    description = "Build and ABI-check the Rust bridge for arm64-v8a and x86_64."
    workingDir(rootProject.projectDir)
    commandLine(rootProject.file("scripts/build-android-rust.sh").absolutePath)
}

val checkNativeAbiPackaging by tasks.registering {
    group = "verification"
    description = "Checks both Android ABIs once native bridge libraries are present."
    inputs.dir(layout.projectDirectory.dir("src/main/jniLibs")).optional()
    doLast {
        val root = inputs.files.files.singleOrNull()
        if (root == null) {
            logger.lifecycle("Native bridge absent: arm64-v8a/x86_64 packaging gate is an explicit placeholder.")
            return@doLast
        }
        val libraries = if (root.isDirectory) {
            root.walkTopDown().filter { it.isFile && it.extension == "so" }.toList()
        } else {
            emptyList()
        }
        if (libraries.isEmpty()) {
            logger.lifecycle("Native bridge absent: arm64-v8a/x86_64 packaging gate is an explicit placeholder.")
            return@doLast
        }
        val byAbi = libraries.groupBy { it.parentFile.name }
        val required = setOf("arm64-v8a", "x86_64")
        check(byAbi.keys.containsAll(required)) { "Native bridge must package arm64-v8a and x86_64" }
        val names = required.associateWith { abi -> byAbi.getValue(abi).map { it.name }.sorted() }
        check(names.getValue("arm64-v8a") == names.getValue("x86_64")) {
            "Native bridge library sets differ between required ABIs"
        }
    }
}

val validateReleaseEvidence by tasks.registering {
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

val cyclonedxBom by tasks.registering {
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

val generateReleaseLicenseReport by tasks.registering {
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

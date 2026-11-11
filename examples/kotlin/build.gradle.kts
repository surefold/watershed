// A small Kotlin program that uses watershed.
//
//     gradle run     builds the Rust library, writes its Kotlin side, runs Main.kt
//     gradle test    the same, then runs the tests
//
// It needs Rust (cargo, 1.88 or later) and a JDK (21 or later). Nothing is
// published anywhere: the Rust library is built from this repository into
// crates/watershed-jvm/target/release, and the JVM is told to look there.

plugins {
    kotlin("jvm") version "2.4.21"
    application
}

repositories {
    mavenCentral()
}

dependencies {
    // The generated Kotlin calls the Rust library through JNA.
    implementation("net.java.dev.jna:jna:5.17.0")
    testImplementation(kotlin("test"))
}

val repository: File = rootDir.resolve("../..").canonicalFile
val bindingsCrate: File = repository.resolve("crates/watershed-jvm")
val libraryFolder: File = bindingsCrate.resolve("target/release")
val library: File = libraryFolder.resolve(System.mapLibraryName("watershed_jvm"))
val generated = layout.buildDirectory.dir("generated/uniffi")

// The engine and its JVM bindings, as one native library.
val buildNative by tasks.registering(Exec::class) {
    workingDir = bindingsCrate
    commandLine("cargo", "build", "--release", "--locked")
    inputs.dir(repository.resolve("crates/watershed-core/src"))
    inputs.dir(repository.resolve("crates/watershed-jvm/src"))
    outputs.file(library)
}

// The Kotlin classes that call it, written by UniFFI from the library itself.
val generateBindings by tasks.registering(Exec::class) {
    dependsOn(buildNative)
    workingDir = bindingsCrate
    commandLine(
        "cargo", "run", "--release", "--locked", "--quiet", "--bin", "uniffi-bindgen", "--",
        "generate", "--library", library.path,
        "--language", "kotlin", "--no-format",
        "--out-dir", generated.get().asFile.path,
    )
    inputs.file(library)
    inputs.file(bindingsCrate.resolve("uniffi.toml"))
    outputs.dir(generated)
}

kotlin {
    sourceSets.main {
        // The generated calls, and the typed layer written over them.
        kotlin.srcDir(generateBindings)
        kotlin.srcDir(bindingsCrate.resolve("kotlin"))
    }
    // Any JDK from 21 up runs this; both compilers aim at 21 so they agree.
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_21
    }
}

java {
    sourceCompatibility = JavaVersion.VERSION_21
    targetCompatibility = JavaVersion.VERSION_21
}

// Where the JVM finds the native library, and leave to call into it.
val nativeAccess = listOf(
    "-Djna.library.path=${libraryFolder.path}",
    "--enable-native-access=ALL-UNNAMED",
)

application {
    mainClass = "MainKt"
    applicationDefaultJvmArgs = nativeAccess
}

tasks.test {
    useJUnitPlatform()
    jvmArgs(nativeAccess)
}

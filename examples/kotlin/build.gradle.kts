// A small Kotlin program that uses watershed.
//
//     gradle run         builds the Rust library and runs Main.kt
//     gradle test        the same, then runs the tests
//     gradle benchmark   times the engine's benchmark graph from Kotlin
//
// It needs Rust (cargo) and a JDK (22 or later). Nothing is published
// anywhere: the Rust library is built from this repository into
// target/release, and the JVM is told to look for it there.

plugins {
    kotlin("jvm") version "2.4.21"
    application
}

repositories {
    mavenCentral()
}

dependencies {
    testImplementation(kotlin("test"))
}

val repository: File = rootDir.resolve("../..").canonicalFile
val bindings: File = repository.resolve("crates/watershed-jvm")
val libraryFolder: File = repository.resolve("target/release")

// The engine, as a C library the JVM calls directly.
val buildNative by tasks.registering(Exec::class) {
    workingDir = repository
    commandLine("cargo", "build", "--release", "--locked", "-p", "watershed-jvm")
    inputs.dir(repository.resolve("crates/watershed-core/src"))
    inputs.dir(bindings.resolve("src"))
    outputs.file(libraryFolder.resolve(System.mapLibraryName("watershed_jvm")))
}

// The JVM side lives beside the Rust crate: one Java class that calls the
// C library, and the typed Kotlin layer over it.
sourceSets.main {
    java.srcDir(bindings.resolve("java"))
}

kotlin {
    sourceSets.main {
        kotlin.srcDir(bindings.resolve("kotlin"))
    }
    // The calls use the JDK's foreign function API, which is in JDK 22 and
    // later; both compilers aim at 22 so they agree.
    compilerOptions {
        jvmTarget = org.jetbrains.kotlin.gradle.dsl.JvmTarget.JVM_22
    }
}

java {
    sourceCompatibility = JavaVersion.VERSION_22
    targetCompatibility = JavaVersion.VERSION_22
}

// Where the JVM finds the library, and leave to call into it.
val nativeAccess = listOf(
    "-Dwatershed.library.path=${libraryFolder.path}",
    "--enable-native-access=ALL-UNNAMED",
)

application {
    mainClass = "MainKt"
    applicationDefaultJvmArgs = nativeAccess
}

tasks.named<JavaExec>("run") {
    dependsOn(buildNative)
}

// The engine's benchmark, driven from Kotlin: `gradle benchmark`.
tasks.register<JavaExec>("benchmark") {
    group = "application"
    description = "Times the engine's benchmark graph, built and updated from Kotlin."
    dependsOn(buildNative)
    classpath = sourceSets.main.get().runtimeClasspath
    mainClass = "BenchmarkKt"
    jvmArgs(nativeAccess)
}

tasks.test {
    dependsOn(buildNative)
    useJUnitPlatform()
    jvmArgs(nativeAccess)
}

// A small Scala 3 program that uses watershed.
//
//     sbt run     builds the Rust library and runs Main.scala
//     sbt test    the same, then runs the tests
//
// It needs Rust (cargo), a JDK (22 or later) and sbt.

import scala.sys.process.*

ThisBuild / scalaVersion := "3.10.1"

val repository = settingKey[File]("The root of the watershed repository.")
val buildNative = taskKey[Unit]("Builds the engine as a C library the JVM calls directly.")

lazy val root = (project in file("."))
  .settings(
    name := "watershed-scala-example",
    repository := baseDirectory.value.getParentFile.getParentFile,
    buildNative := {
      val log = streams.value.log
      val command = Seq("cargo", "build", "--release", "--locked", "-p", "watershed-jvm")
      if (Process(command, repository.value).!(log) != 0) sys.error("building the engine failed")
    },
    Compile / compile := (Compile / compile).dependsOn(buildNative).value,
    // The JVM side lives beside the Rust crate: one Java class that calls
    // the C library, and the typed Scala layer over it.
    Compile / unmanagedSourceDirectories ++= Seq(
      repository.value / "crates" / "watershed-jvm" / "java",
      repository.value / "crates" / "watershed-jvm" / "scala",
    ),
    javacOptions ++= Seq("--release", "22"),
    libraryDependencies += "org.scalameta" %% "munit" % "1.3.6" % Test,
    // Where the JVM finds the library, and leave to call into it.
    fork := true,
    javaOptions ++= Seq(
      s"-Dwatershed.library.path=${repository.value / "target" / "release"}",
      "--enable-native-access=ALL-UNNAMED",
    ),
    scalacOptions ++= Seq("-deprecation", "-feature", "-Werror"),
  )

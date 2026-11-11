// A small Scala 3 program that uses watershed.
//
//     sbt run     builds the Rust library and its JVM bindings, runs Main.scala
//     sbt test    the same, then runs the tests
//
// It needs Rust (cargo, 1.88 or later), a JDK (21 or later), Gradle and sbt.
// Gradle is there because the bindings are generated as Kotlin (by UniFFI)
// and compiled in ../kotlin; this build takes the classes from there.

import scala.sys.process.*

ThisBuild / scalaVersion := "3.10.1"

val repository = settingKey[File]("The root of the watershed repository.")
val bindingsJar = taskKey[File]("The engine's JVM bindings, built by ../kotlin.")

lazy val root = (project in file("."))
  .settings(
    name := "watershed-scala-example",
    repository := baseDirectory.value.getParentFile.getParentFile,
    // The Kotlin project builds the native library, generates the bindings
    // and compiles them. Gradle does nothing when all of that is up to date.
    bindingsJar := {
      val kotlin = repository.value / "examples" / "kotlin"
      val log = streams.value.log
      val status = Process(Seq("gradle", "--quiet", "--console=plain", "jar"), kotlin).!(log)
      if (status != 0) sys.error("building the bindings in examples/kotlin failed")
      kotlin / "build" / "libs" / "watershed-kotlin-example.jar"
    },
    Compile / unmanagedJars += Attributed.blank(bindingsJar.value),
    // The typed Scala layer over the bindings lives beside the Rust crate.
    Compile / unmanagedSourceDirectories += repository.value / "crates" / "watershed-jvm" / "scala",
    libraryDependencies ++= Seq(
      // The bindings call the Rust library through JNA, and are Kotlin underneath.
      "net.java.dev.jna" % "jna" % "5.17.0",
      "org.jetbrains.kotlin" % "kotlin-stdlib" % "2.4.21",
      "org.scalameta" %% "munit" % "1.3.6" % Test,
    ),
    // Where the JVM finds the native library, and leave to call into it.
    fork := true,
    javaOptions ++= Seq(
      s"-Djna.library.path=${repository.value / "crates" / "watershed-jvm" / "target" / "release"}",
      "--enable-native-access=ALL-UNNAMED",
    ),
    scalacOptions ++= Seq("-deprecation", "-feature", "-Werror"),
  )

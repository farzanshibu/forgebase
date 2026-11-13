import org.jetbrains.kotlin.gradle.dsl.JvmTarget
import org.jetbrains.kotlin.gradle.dsl.KotlinVersion

plugins {
    kotlin("jvm") version "2.2.20"
    kotlin("plugin.serialization") version "2.2.20"
    `java-library`
    `maven-publish`
}

group = "dev.forgebase"
version = "0.1.0"

// CI uses the default build directory; local agents with little disk set FORGEBASE_SDK_BUILD_DIR.
System.getenv("FORGEBASE_SDK_BUILD_DIR")?.takeIf { it.isNotBlank() }?.let {
    layout.buildDirectory.set(file("$it/${project.name}"))
}

val coroutines = "1.10.2"
val serialization = "1.9.0"
val okhttp = "4.12.0"

dependencies {
    api("org.jetbrains.kotlinx:kotlinx-coroutines-core:$coroutines")
    api("org.jetbrains.kotlinx:kotlinx-serialization-json:$serialization")
    api("com.squareup.okhttp3:okhttp:$okhttp")

    testImplementation(kotlin("test"))
    testImplementation("org.jetbrains.kotlinx:kotlinx-coroutines-test:$coroutines")
    testImplementation("com.squareup.okhttp3:mockwebserver:$okhttp")
}

java {
    sourceCompatibility = JavaVersion.VERSION_17
    targetCompatibility = JavaVersion.VERSION_17
    withSourcesJar()
}

kotlin {
    explicitApi()
    jvmToolchain(17)
    compilerOptions {
        jvmTarget.set(JvmTarget.JVM_17)
        apiVersion.set(KotlinVersion.KOTLIN_2_0)
        languageVersion.set(KotlinVersion.KOTLIN_2_0)
        allWarningsAsErrors.set(true)
        freeCompilerArgs.addAll("-Xjvm-default=all")
    }
}

tasks.test {
    useJUnitPlatform()
    testLogging {
        events("passed", "skipped", "failed")
        showStandardStreams = false
        exceptionFormat = org.gradle.api.tasks.testing.logging.TestExceptionFormat.FULL
    }
}

publishing {
    publications {
        create<MavenPublication>("maven") {
            from(components["java"])
            artifactId = "forgebase"
            pom {
                name.set("ForgeBase Kotlin SDK")
                description.set("Kotlin (JVM and Android) client for ForgeBase.")
                licenses {
                    license { name.set("Apache-2.0") }
                    license { name.set("MIT") }
                }
            }
        }
    }
}

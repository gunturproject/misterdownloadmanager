package com.misterdownloadmanager.android

import android.Manifest
import android.app.Application
import android.content.pm.PackageManager
import android.os.Build
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.compose.setContent
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.content.ContextCompat
import androidx.lifecycle.AndroidViewModel
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel

class MainActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val initialUrl = intent?.dataString.orEmpty()
        setContent {
            MaterialTheme(colorScheme = androidx.compose.material3.darkColorScheme()) {
                DownloadScreen(initialUrl)
            }
        }
    }
}

class DownloadViewModel(app: Application) : AndroidViewModel(app) {
    private val repository = DownloadRepository.get(app)
    val items = repository.items

    fun add(url: String, connections: Int) = repository.add(url, connections)
    fun pause(id: Long) = repository.pause(id)
    fun resume(id: Long) = repository.resume(id)
    fun remove(id: Long) = repository.remove(id)
}

@Composable
fun DownloadScreen(initialUrl: String, viewModel: DownloadViewModel = viewModel()) {
    val items by viewModel.items.collectAsStateWithLifecycle()
    val context = LocalContext.current
    var url by remember { mutableStateOf(initialUrl) }
    var connections by remember { mutableStateOf("8") }

    val requestNotifications = rememberLauncherForActivityResult(
        ActivityResultContracts.RequestPermission(),
    ) { /* The download does not depend on the notification being granted. */ }

    LaunchedEffect(Unit) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            ContextCompat.checkSelfPermission(
                context,
                Manifest.permission.POST_NOTIFICATIONS,
            ) != PackageManager.PERMISSION_GRANTED
        ) {
            requestNotifications.launch(Manifest.permission.POST_NOTIFICATIONS)
        }
    }

    Column(modifier = Modifier.fillMaxSize().padding(16.dp), verticalArrangement = Arrangement.spacedBy(12.dp)) {
        Text("Mister Download Manager", style = MaterialTheme.typography.headlineSmall)
        OutlinedTextField(
            value = url,
            onValueChange = { url = it },
            label = { Text("Download URL (http/https)") },
            modifier = Modifier.fillMaxWidth(),
            singleLine = true,
        )
        Row(horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            OutlinedTextField(
                value = connections,
                onValueChange = { connections = it },
                label = { Text("Connections") },
                modifier = Modifier.weight(1f),
                singleLine = true,
            )
            Button(
                onClick = {
                    val n = connections.toIntOrNull()?.coerceIn(1, 16) ?: 8
                    if (url.isNotBlank()) viewModel.add(url.trim(), n)
                },
                modifier = Modifier.padding(top = 8.dp),
            ) { Text("Add") }
        }

        LazyColumn(verticalArrangement = Arrangement.spacedBy(8.dp)) {
            items(items, key = { it.id }) { item ->
                Card(modifier = Modifier.fillMaxWidth()) {
                    Column(Modifier.padding(12.dp), verticalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text(item.name, style = MaterialTheme.typography.titleMedium)
                        Text("${item.state} · ${formatBytes(item.downloaded)} / ${formatBytes(item.total)} · ${formatBytes(item.speed)}/s")
                        if (item.total > 0 && !item.finished) {
                            LinearProgressIndicator(
                                progress = { (item.downloaded.toFloat() / item.total.toFloat()).coerceIn(0f, 1f) },
                                modifier = Modifier.fillMaxWidth(),
                            )
                        } else if (!item.finished) {
                            LinearProgressIndicator(modifier = Modifier.fillMaxWidth())
                        }
                        item.error?.let { Text(it, color = MaterialTheme.colorScheme.error) }
                        Row {
                            if (item.finished) {
                                TextButton(onClick = { viewModel.remove(item.id) }) { Text("Remove") }
                            } else if (item.paused) {
                                TextButton(onClick = { viewModel.resume(item.id) }) { Text("Resume") }
                                TextButton(onClick = { viewModel.remove(item.id) }) { Text("Remove") }
                            } else {
                                TextButton(onClick = { viewModel.pause(item.id) }) { Text("Pause") }
                                TextButton(onClick = { viewModel.remove(item.id) }) { Text("Remove") }
                            }
                        }
                    }
                }
            }
        }
    }
}

private fun formatBytes(bytes: Long): String = when {
    bytes <= 0 -> "0 B"
    bytes < 1024 -> "$bytes B"
    bytes < 1024 * 1024 -> "${bytes / 1024} KB"
    bytes < 1024 * 1024 * 1024 -> "${bytes / (1024 * 1024)} MB"
    else -> String.format("%.2f GB", bytes / (1024.0 * 1024.0 * 1024.0))
}
